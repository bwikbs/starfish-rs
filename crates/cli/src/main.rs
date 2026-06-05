//! starfish CLI — `starfish render <input.html> -o <out.png> [--width N]`.

use std::process::ExitCode;

use starfish_net::{base_url_from_input, LoadError, LocalLoader, ResourceLoader};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("starfish: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: starfish render <input.html> -o <out.png> [--width N]";

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

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                output = Some(iter.next().ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))?);
            }
            "--width" => {
                let v = iter.next().ok_or_else(|| format!("--width needs a value\n{USAGE}"))?;
                width = v.parse().map_err(|_| format!("invalid --width '{v}'"))?;
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

    // Fetch the document itself through the loader (file:// only in M1).
    let loader = LocalLoader;
    let bytes = match loader.fetch(&base) {
        Ok(res) => res.bytes,
        Err(LoadError::UnsupportedScheme(s)) => {
            return Err(format!("{s}:// URLs are not supported yet (HTTP lands in E3-M2)"));
        }
        Err(e) => return Err(format!("reading {input}: {e}")),
    };
    let html = String::from_utf8_lossy(&bytes);

    let pixmap = starfish_paint::render_document(&html, &base, width as f32, &loader);
    pixmap
        .save_png(&output)
        .map_err(|e| format!("writing {output}: {e}"))?;

    println!("wrote {output} ({}x{})", pixmap.width(), pixmap.height());
    Ok(())
}
