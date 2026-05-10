//! Panel test mode: render every paint colour in the panel palette as
//! full-height or full-width bands. Entered with a three-button boot hold.

use alloc::vec::Vec;

use embassy_time::Duration;
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::prelude::{Point, Primitive, Size};
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_text::TextBox;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::spi::master::Spi;
use esp_println::println;

use crate::app::AppContext;
use crate::button::wait_for_press;
use crate::canvas::Canvas;
use crate::panel::{Panel, PanelColor};

pub async fn run<P>(mut ctx: AppContext<P>) -> !
where
    P: Panel<Spi<'static, esp_hal::Async>>,
{
    println!("Test mode");
    println!("Test mode - beep");
    ctx.buzzer.beep(Duration::from_millis(80)).await;

    let frame = render_swatch::<P::Color>(P::WIDTH, P::HEIGHT, ctx.refresh_button_label);
    let init_mode = P::init_mode_for_palette(P::Color::all());

    ctx.epd.enable().await.unwrap();
    println!("Reset");
    ctx.epd.reset().await.unwrap();
    println!("Wait until idle");
    ctx.epd.wait_until_idle().await.unwrap();
    println!("Init");
    ctx.epd.init(&mut ctx.spi_bus, init_mode).await.unwrap();
    println!("Power on");
    ctx.epd.power_on(&mut ctx.spi_bus).await.unwrap();
    println!("Update frame (test swatch)");
    let data = (0..(P::WIDTH * P::HEIGHT)).map(|idx| {
        let (x, y) = P::output_index_to_image_xy(idx);
        frame[y * P::WIDTH + x]
    });
    ctx.epd.update_frame(&mut ctx.spi_bus, data).await.unwrap();
    println!("Trigger refresh");
    ctx.epd
        .display_frame_no_wait(&mut ctx.spi_bus)
        .await
        .unwrap();
    println!("Wait until idle (~20s refresh)");
    ctx.epd.wait_until_idle().await.unwrap();
    println!("Power off");
    ctx.epd.power_off(&mut ctx.spi_bus).await.unwrap();
    ctx.epd.disable().await.unwrap();

    println!("Test mode done — press Refresh to reboot");
    let mut refresh_input = Input::new(
        ctx.gpio_btn_refresh,
        InputConfig::default().with_pull(Pull::Up),
    );
    refresh_input.wait_for_high().await;
    wait_for_press(&mut refresh_input, Duration::from_millis(50)).await;
    println!("Rebooting");
    esp_hal::system::software_reset();
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

fn draw_instructions<C: PanelColor>(
    canvas: &mut Canvas<C>,
    width: u32,
    height: u32,
    refresh_button_label: &str,
) {
    const BOX_PADDING_PX: u32 = 24;
    let text = alloc::format!(
        "Test mode active\nDevice remains powered on\nPress {} to reboot",
        refresh_button_label
    );
    let text_width = text_width_px(&text, &FONT_10X20);
    let text_height = text_height_px(&text, &FONT_10X20);
    let max_text_width = width.saturating_sub(2 * BOX_PADDING_PX);
    let text_width = text_width.min(max_text_width);

    let box_width = (text_width + 2 * BOX_PADDING_PX).min(width);
    let box_height = (text_height + 2 * BOX_PADDING_PX).min(height);
    let box_x = ((width - box_width) / 2) as i32;
    let box_y = ((height - box_height) / 2) as i32;
    let box_area = Rectangle::new(Point::new(box_x, box_y), Size::new(box_width, box_height));

    let _ = box_area
        .into_styled(PrimitiveStyle::with_fill(C::WHITE))
        .draw(canvas);
    let _ = box_area
        .into_styled(PrimitiveStyle::with_stroke(C::BLACK, 2))
        .draw(canvas);

    let text_width = box_width.saturating_sub(2 * BOX_PADDING_PX);
    let text_height = box_height.saturating_sub(2 * BOX_PADDING_PX);
    let text_area = Rectangle::new(
        Point::new(box_x + BOX_PADDING_PX as i32, box_y + BOX_PADDING_PX as i32),
        Size::new(text_width, text_height),
    );
    let style = MonoTextStyle::new(&FONT_10X20, C::BLACK);
    let _ = TextBox::new(&text, text_area, style).draw(canvas);
}

fn text_width_px(text: &str, font: &MonoFont<'_>) -> u32 {
    let char_width = font.character_size.width;
    text.lines()
        .map(|line| line.chars().count() as u32 * char_width)
        .max()
        .unwrap_or(0)
}

fn text_height_px(text: &str, font: &MonoFont<'_>) -> u32 {
    let line_count = text.lines().count().max(1) as u32;
    line_count * font.character_size.height
}
