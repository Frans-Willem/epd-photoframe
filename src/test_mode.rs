//! Panel test mode: render every paint colour in the panel palette as
//! full-height or full-width bands. Entered with a three-button boot hold.

use alloc::format;
use alloc::vec::Vec;

use embassy_time::Duration;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::spi::master::Spi;
use esp_hal::uart::UartRx;
use esp_println::println;

use crate::app::AppContext;
use crate::button::wait_for_press;
use crate::canvas::Canvas;
use crate::panel::{Panel, PanelColor};
use crate::text_box;

pub async fn run<P>(ctx: AppContext<P>) -> !
where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    let refresh_button_label = ctx.refresh_button_label;
    let mut buzzer = ctx.buzzer;
    let mut gpio_btn_refresh = ctx.gpio_btn_refresh;
    let mut spi_bus = ctx.spi_bus;
    let mut epd = ctx.epd;
    let mut uart_rx = ctx.uart_rx;

    println!("Test mode");
    println!("Test mode - beep");
    buzzer.beep(Duration::from_millis(80)).await;

    let mut refresh_input = Input::new(
        gpio_btn_refresh.reborrow(),
        InputConfig::default().with_pull(Pull::Up),
    );
    let refresh_pressed = async {
        refresh_input.wait_for_high().await;
        wait_for_press(&mut refresh_input, Duration::from_millis(50)).await;
    };

    let _ = embassy_futures::select::select(
        refresh_pressed,
        main_loop::<P>(&mut spi_bus, &mut epd, &mut uart_rx, refresh_button_label),
    )
    .await;
    println!("Rebooting");
    esp_hal::system::software_reset();
}

async fn main_loop<P>(
    spi_bus: &mut Spi<'static, esp_hal::Async>,
    epd: &mut P,
    uart_rx: &mut UartRx<'static, esp_hal::Async>,
    refresh_button_label: &'static str,
) where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    let colors: Vec<P::Color> = P::Color::all().collect();
    render_test_frame::<P>(
        spi_bus,
        epd,
        render_swatch::<P::Color>(P::WIDTH, P::HEIGHT, refresh_button_label),
        "test swatch",
    )
    .await;

    println!("Test mode ready. Send `COLOR number` over UART, or press Refresh to reboot.");
    loop {
        match read_command(uart_rx).await {
            TestCommand::Color(index) => {
                if let Some(color) = colors.get(index).copied() {
                    println!("Rendering COLOR {}", index);
                    render_test_frame::<P>(
                        spi_bus,
                        epd,
                        render_solid_color::<P::Color>(
                            P::WIDTH,
                            P::HEIGHT,
                            color,
                            refresh_button_label,
                        ),
                        "solid color",
                    )
                    .await;
                    println!("Test mode ready.");
                } else {
                    println!(
                        "Unknown COLOR index {}. Valid range: 0..{}",
                        index,
                        colors.len()
                    );
                }
            }
        }
    }
}

fn render_swatch<C: PanelColor>(width: usize, height: usize, refresh_button_label: &str) -> Vec<C> {
    let colors: Vec<C> = C::all().collect();
    let fallback = C::WHITE;
    let count = colors.len().max(1);

    let mut canvas = Canvas::new_with(width as u32, height as u32, |x, y| {
        let (x, y) = (x as usize, y as usize);
        let band = if width >= height {
            x * count / width
        } else {
            y * count / height
        };
        colors.get(band).copied().unwrap_or(fallback)
    });

    draw_instructions(
        &mut canvas,
        width as u32,
        height as u32,
        refresh_button_label,
    );
    canvas.into_vec()
}

fn render_solid_color<C: PanelColor>(
    width: usize,
    height: usize,
    color: C,
    refresh_button_label: &str,
) -> Vec<C> {
    let mut canvas = Canvas::new(width as u32, height as u32, color);
    draw_instructions(
        &mut canvas,
        width as u32,
        height as u32,
        refresh_button_label,
    );
    canvas.into_vec()
}

async fn render_test_frame<P>(
    spi_bus: &mut Spi<'static, esp_hal::Async>,
    epd: &mut P,
    frame: Vec<P::Color>,
    label: &str,
) where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    let init_mode = P::init_mode_for_palette(P::Color::all());

    epd.enable().await.unwrap();
    println!("Reset");
    epd.reset().await.unwrap();
    println!("Wait until idle");
    epd.wait_until_idle().await.unwrap();
    println!("Init");
    epd.init(spi_bus, init_mode).await.unwrap();
    println!("Power on");
    epd.power_on(spi_bus).await.unwrap();
    println!("Update frame ({})", label);
    let data = (0..(P::WIDTH * P::HEIGHT)).map(|idx| {
        let (x, y) = P::output_index_to_image_xy(idx);
        frame[y * P::WIDTH + x]
    });
    epd.update_frame(spi_bus, data).await.unwrap();
    println!("Trigger refresh");
    epd.display_frame_no_wait(spi_bus).await.unwrap();
    println!("Wait until idle (~20s refresh)");
    epd.wait_until_idle().await.unwrap();
    println!("Power off");
    epd.power_off(spi_bus).await.unwrap();
    epd.disable().await.unwrap();
}

enum TestCommand {
    Color(usize),
}

async fn read_command(uart_rx: &mut UartRx<'static, esp_hal::Async>) -> TestCommand {
    let mut line = heapless::String::<64>::new();
    loop {
        if let Some(byte) = read_byte(uart_rx).await
            && let Some(command) = handle_command_byte(byte, &mut line)
        {
            return command;
        }
    }
}

fn handle_command_byte(byte: u8, line: &mut heapless::String<64>) -> Option<TestCommand> {
    match byte {
        b'\n' => {
            if let Some(command) = parse_command(line.trim()) {
                return Some(command);
            }
            println!("Unknown test command: {:?}", line.as_str());
            line.clear();
        }
        b'\r' => {}
        byte => {
            if line.push(byte as char).is_err() {
                println!("Test command too long; clearing input");
                line.clear();
            }
        }
    }

    None
}

async fn read_byte(uart_rx: &mut UartRx<'static, esp_hal::Async>) -> Option<u8> {
    let mut byte = [0];
    match uart_rx.read_async(&mut byte).await {
        Ok(1) => Some(byte[0]),
        Ok(_) => None,
        Err(e) => {
            println!("UART RX error: {:?}", e);
            None
        }
    }
}

fn parse_command(line: &str) -> Option<TestCommand> {
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(command), Some(index), None) if command.eq_ignore_ascii_case("COLOR") => {
            index.parse().ok().map(TestCommand::Color)
        }
        _ => None,
    }
}

fn draw_instructions<C: PanelColor>(
    canvas: &mut Canvas<C>,
    width: u32,
    height: u32,
    refresh_button_label: &str,
) {
    let text = format!(
        "Test mode active\nDevice remains powered on\nPress {} to reboot",
        refresh_button_label
    );
    text_box::draw_centered(canvas, width, height, &text);
}
