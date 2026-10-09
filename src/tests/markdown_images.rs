use super::{test_assets, test_md_theme};
use crate::markdown::{
    fetch_remote_image, line_plain_text, parse_markdown_with_width,
    replace_images_with_placeholders,
};
use image::{Rgb, RgbImage};
use ratatui_image::picker::{Picker, ProtocolType};
use std::path::PathBuf;

fn picker_with_protocol(protocol: ProtocolType) -> Picker {
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(protocol);
    picker
}

fn write_test_png(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "leaf-image-test-{}-{}",
        std::process::id(),
        name.replace('.', "_")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    RgbImage::from_pixel(20, 20, Rgb([200, 50, 50]))
        .save(&path)
        .expect("write test png");
    (dir, path)
}

#[test]
fn local_image_is_decoded_and_reserves_lines() {
    let (ss, theme) = test_assets();
    let (dir, _path) = write_test_png("pic.png");

    let src = "![a pic](pic.png)\n";
    let picker = picker_with_protocol(ProtocolType::Kitty);
    let parsed = parse_markdown_with_width(
        src,
        &ss,
        &theme,
        40,
        &test_md_theme(),
        false,
        true,
        Some(dir.as_path()),
        &picker,
    );

    assert_eq!(parsed.images.len(), 1);
    let entry = &parsed.images[0];
    let height = entry.slice.size().height as usize;
    assert!(height > 0);
    assert!(parsed.lines.len() >= entry.rendered_start + height);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn halfblocks_only_terminal_still_renders_image() {
    let (ss, theme) = test_assets();
    let (dir, _path) = write_test_png("halfblock.png");

    let src = "![a pic](halfblock.png)\n";
    let picker = Picker::halfblocks();
    let parsed = parse_markdown_with_width(
        src,
        &ss,
        &theme,
        40,
        &test_md_theme(),
        false,
        true,
        Some(dir.as_path()),
        &picker,
    );

    assert_eq!(parsed.images.len(), 1);
    assert!(parsed.images[0].slice.size().height > 0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn inline_output_collapses_images_to_placeholders() {
    let (ss, theme) = test_assets();
    let (dir, _path) = write_test_png("inline.png");

    let src = "before\n\n![a pic](inline.png)\n\nafter\n";
    let picker = Picker::halfblocks();
    let parsed = parse_markdown_with_width(
        src,
        &ss,
        &theme,
        40,
        &test_md_theme(),
        false,
        true,
        Some(dir.as_path()),
        &picker,
    );
    let height = parsed.images[0].slice.size().height as usize;
    let before_len = parsed.lines.len();

    let lines = replace_images_with_placeholders(parsed.lines, &parsed.images);

    assert_eq!(lines.len(), before_len - height + 1);
    let text: Vec<String> = lines.iter().map(line_plain_text).collect();
    assert!(text.iter().any(|l| l == "[img: a pic]"));
    assert!(text.iter().any(|l| l.contains("before")));
    assert!(text.iter().any(|l| l.contains("after")));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_image_falls_back_to_placeholder() {
    let (ss, theme) = test_assets();
    let src = "![missing alt](does-not-exist.png)\n";
    let picker = picker_with_protocol(ProtocolType::Kitty);
    let parsed = parse_markdown_with_width(
        src,
        &ss,
        &theme,
        40,
        &test_md_theme(),
        false,
        true,
        None,
        &picker,
    );

    assert!(parsed.images.is_empty());
    let text: String = parsed
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.contains("[img: missing alt]"));
}

#[test]
fn remote_image_url_falls_back_to_placeholder_without_fetching() {
    let (ss, theme) = test_assets();
    let src = "![remote](https://example.com/a.png)\n";
    let picker = picker_with_protocol(ProtocolType::Kitty);
    let parsed = parse_markdown_with_width(
        src,
        &ss,
        &theme,
        40,
        &test_md_theme(),
        false,
        true,
        None,
        &picker,
    );

    assert!(parsed.images.is_empty());
    let text: String = parsed
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.contains("[img: remote]"));
}

/// Serves one HTTP response on a loopback port and returns its base URL.
fn serve_once(status: &str, body: Vec<u8>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let status = status.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });
    format!("http://{addr}")
}

#[test]
fn remote_image_is_downloaded_and_decodes() {
    let mut png = Vec::new();
    RgbImage::from_pixel(8, 8, Rgb([10, 200, 10]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let url = serve_once("200 OK", png.clone());

    let bytes = fetch_remote_image(&format!("{url}/pic.png")).expect("download");

    assert_eq!(bytes, png);
    assert!(image::load_from_memory(&bytes).is_ok());
}

#[test]
fn remote_image_http_error_yields_none() {
    let url = serve_once("404 Not Found", Vec::new());
    assert!(fetch_remote_image(&format!("{url}/missing.png")).is_none());
}
