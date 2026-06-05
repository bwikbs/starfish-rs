//! starfish CLI — `starfish render <input.html> -o <out.png> [--width N]`.

use std::process::ExitCode;
use std::time::Duration;

use starfish_net::{base_url_from_input, LoadError, ResourceLoader, RouterLoader};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("starfish: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str =
    "usage: starfish render <url|path> -o <out.png> [--width N] [--timeout S]";

fn run(args: Vec<String>) -> Result<(), String> {
    let mut iter = args.into_iter();

    match iter.next().as_deref() {
        Some("render") => {}
        Some(other) => return Err(format!("unknown command '{other}'\n{USAGE}")),
        None => return Err(format!("missing command\n{USAGE}")),
    }

    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut width: u32 = 800;
    let mut timeout: Option<Duration> = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                output = Some(iter.next().ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))?);
            }
            "--width" => {
                let v = iter.next().ok_or_else(|| format!("--width needs a value\n{USAGE}"))?;
                width = v.parse().map_err(|_| format!("invalid --width '{v}'"))?;
            }
            "--timeout" => {
                let v = iter.next().ok_or_else(|| format!("--timeout needs a value\n{USAGE}"))?;
                let secs: u64 = v.parse().map_err(|_| format!("invalid --timeout '{v}'"))?;
                timeout = Some(Duration::from_secs(secs));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag '{flag}'\n{USAGE}"));
            }
            positional => {
                if input.is_some() {
                    return Err(format!("unexpected argument '{positional}'\n{USAGE}"));
                }
                input = Some(positional.to_string());
            }
        }
    }

    let input = input.ok_or_else(|| format!("missing <input.html>\n{USAGE}"))?;
    let output = output.ok_or_else(|| format!("missing -o <out.png>\n{USAGE}"))?;

    // A path or a `file://`/`http(s)://` URL → the document's base URL. Relative
    // `<link href>`/`<img src>` later resolve against this.
    let base = base_url_from_input(&input).map_err(|e| format!("bad input '{input}': {e}"))?;

    // One router handles file:// and http(s):// — for the document AND its
    // resources (linked CSS/images resolve back through the same loader).
    let loader = match timeout {
        Some(t) => RouterLoader::with_timeout(t),
        None => RouterLoader::new(),
    };
    let (bytes, final_url) = match loader.fetch(&base) {
        Ok(res) => (res.bytes, res.final_url),
        Err(LoadError::UnsupportedScheme(s)) => {
            return Err(format!("{s}:// URLs are not supported (use file/http/https)"));
        }
        Err(e) => return Err(format!("fetching {input}: {e}")),
    };
    let html = String::from_utf8_lossy(&bytes);

    // Resolve relative sub-resources against the final (post-redirect) URL so a
    // 302 doesn't drop the page's relative CSS/images.
    let render_base = final_url.as_ref().unwrap_or(&base);
    let pixmap = starfish_paint::render_document(&html, render_base, width as f32, &loader);
    pixmap
        .save_png(&output)
        .map_err(|e| format!("writing {output}: {e}"))?;

    println!("wrote {output} ({}x{})", pixmap.width(), pixmap.height());
    Ok(())
}
