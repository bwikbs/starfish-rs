//! starfish CLI — `starfish render <input.html> -o <out.png> [--width N]`.

use std::process::ExitCode;
use std::time::Duration;

use starfish_net::{base_url_from_input, CachingLoader, LoadError, ResourceLoader, RouterLoader};

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
    "usage: starfish render <url|path> -o <out.png> [--width N] [--timeout S] [--at SECONDS] \
     [--color-scheme light|dark] [--reduced-motion] [--pointer fine|coarse|none]";

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
    let mut at_seconds: f32 = 0.0;
    let mut prefs = starfish_paint::MediaPrefs::default();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                output = Some(
                    iter.next()
                        .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))?,
                );
            }
            "--width" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("--width needs a value\n{USAGE}"))?;
                width = v.parse().map_err(|_| format!("invalid --width '{v}'"))?;
            }
            "--timeout" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("--timeout needs a value\n{USAGE}"))?;
                let secs: u64 = v.parse().map_err(|_| format!("invalid --timeout '{v}'"))?;
                timeout = Some(Duration::from_secs(secs));
            }
            "--at" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("--at needs a value\n{USAGE}"))?;
                at_seconds = v.parse().map_err(|_| format!("invalid --at '{v}'"))?;
            }
            // E27-M2: @media user-preference flags.
            "--color-scheme" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("--color-scheme needs a value\n{USAGE}"))?;
                prefs.color_scheme = match v.as_str() {
                    "light" => starfish_paint::ColorScheme::Light,
                    "dark" => starfish_paint::ColorScheme::Dark,
                    _ => return Err(format!("invalid --color-scheme '{v}' (light|dark)")),
                };
            }
            "--reduced-motion" => {
                prefs.reduced_motion = true;
            }
            "--pointer" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("--pointer needs a value\n{USAGE}"))?;
                prefs.pointer = match v.as_str() {
                    "fine" => starfish_paint::PointerKind::Fine,
                    "coarse" => starfish_paint::PointerKind::Coarse,
                    "none" => starfish_paint::PointerKind::None,
                    _ => return Err(format!("invalid --pointer '{v}' (fine|coarse|none)")),
                };
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
    let router = match timeout {
        Some(t) => RouterLoader::with_timeout(t),
        None => RouterLoader::new(),
    };
    // Dedupe document/CSS/image fetches by URL within this render. The document
    // fetch below is a cache miss, so its original error variant (e.g.
    // UnsupportedScheme) is preserved for the friendly-message mapping.
    let loader = CachingLoader::new(router);
    let (bytes, final_url) = match loader.fetch(&base) {
        Ok(res) => (res.bytes, res.final_url),
        Err(LoadError::UnsupportedScheme(s)) => {
            return Err(format!(
                "{s}:// URLs are not supported (use file/http/https)"
            ));
        }
        Err(e) => return Err(format!("fetching {input}: {e}")),
    };
    let html = String::from_utf8_lossy(&bytes);

    // Resolve relative sub-resources against the final (post-redirect) URL so a
    // 302 doesn't drop the page's relative CSS/images.
    let render_base = final_url.as_ref().unwrap_or(&base);
    let pixmap = starfish_paint::render_document_at_prefs(
        &html,
        render_base,
        width as f32,
        &loader,
        at_seconds,
        prefs,
    );
    pixmap
        .save_png(&output)
        .map_err(|e| format!("writing {output}: {e}"))?;

    println!("wrote {output} ({}x{})", pixmap.width(), pixmap.height());
    Ok(())
}
