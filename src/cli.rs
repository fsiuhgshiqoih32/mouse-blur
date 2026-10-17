//! Command-line options.

use std::path::PathBuf;

use mouse_blur::config::{Lag, Settings, Style, parse_color};

#[derive(Debug, Default)]
pub struct Cli {
    pub config: Option<PathBuf>,
    pub no_tray: bool,
    pub help: bool,
    pub version: bool,
    pub print_config_path: bool,
    /// Hidden: drive the real overlay with a simulated pointer for 2 s and report (used by CI).
    pub self_test: bool,
    pub overrides: Overrides,
}

/// Values given on the command line; they override the config file for this run.
#[derive(Debug, Default)]
pub struct Overrides {
    intensity: Option<u32>,
    blur_ms: Option<u32>,
    taper: Option<u32>,
    lag: Option<Lag>,
    min_speed: Option<u32>,
    style: Option<Style>,
    color: Option<String>,
    hotkey: Option<String>,
    disabled: bool,
}

impl Overrides {
    pub fn apply(&self, s: &mut Settings) {
        if let Some(v) = self.intensity {
            s.intensity = v;
        }
        if let Some(v) = self.blur_ms {
            s.blur_ms = v;
        }
        if let Some(v) = self.taper {
            s.taper = v;
        }
        if let Some(v) = self.lag {
            s.lag_ms = v;
        }
        if let Some(v) = self.min_speed {
            s.min_speed = v;
        }
        if let Some(v) = self.style {
            s.style = v;
        }
        if let Some(v) = &self.color {
            s.color = v.clone();
        }
        if let Some(v) = &self.hotkey {
            s.hotkey = v.clone();
        }
        if self.disabled {
            s.enabled = false;
        }
    }
}

pub fn help() -> String {
    format!(
        "Mouse Blur {}\n\
         Real motion blur for your mouse pointer.\n\
         \n\
         Usage: mouse-blur [options]\n\
         \n\
         Options:\n\
         \x20 --intensity <percent>   How visible the smear is; 100 is physically accurate [300]\n\
         \x20 --blur <ms>             Shutter time: how far back the smear reaches [50]\n\
         \x20 --taper <percent>       How much the smear fades towards its tail [50]\n\
         \x20 --lag <ms|auto>         Lag compensation [auto]\n\
         \x20 --min-speed <px/s>      Pointer speed below which there is no smear [150]\n\
         \x20 --style <name>          natural, tint or rainbow [natural]\n\
         \x20 --color <#RRGGBB>       Trail colour for --style tint\n\
         \x20 --hotkey <keys>         Toggle shortcut, e.g. Ctrl+Alt+B; \"\" disables it\n\
         \x20 --disabled              Start with the effect turned off\n\
         \x20 --no-tray               Run without a tray icon\n\
         \x20 --config <path>         Use this config file instead of the default\n\
         \x20 --print-config-path     Print where the config file lives and exit\n\
         \x20 -h, --help              Show this help\n\
         \x20 -V, --version           Show the version\n\
         \n\
         Options override the config file for this run. Changes made from the tray menu\n\
         are written to the config file, which is also reloaded automatically when edited.\n",
        env!("CARGO_PKG_VERSION")
    )
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            inline.clone().or_else(|| args.next()).ok_or_else(|| format!("{name} needs {what}"))
        };
        let number = |v: String, name: &str| {
            v.trim().parse::<u32>().map_err(|_| format!("{name}: expected a number, got {v:?}"))
        };
        match name.as_str() {
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
            "--no-tray" => cli.no_tray = true,
            "--disabled" => cli.overrides.disabled = true,
            "--print-config-path" => cli.print_config_path = true,
            "--self-test" => cli.self_test = true,
            "--config" => cli.config = Some(PathBuf::from(value("a path")?)),
            "--intensity" => cli.overrides.intensity = Some(number(value("a percentage")?, &name)?),
            "--blur" => cli.overrides.blur_ms = Some(number(value("milliseconds")?, &name)?),
            "--taper" => cli.overrides.taper = Some(number(value("a percentage")?, &name)?),
            "--min-speed" => cli.overrides.min_speed = Some(number(value("pixels per second")?, &name)?),
            "--lag" => {
                let v = value("milliseconds or \"auto\"")?;
                cli.overrides.lag =
                    Some(Lag::parse(&v).ok_or_else(|| format!("--lag: expected ms or auto, got {v:?}"))?);
            }
            "--style" => {
                let v = value("a style name")?;
                cli.overrides.style = Some(
                    Style::parse(&v).ok_or_else(|| format!("--style: expected natural, tint or rainbow, got {v:?}"))?,
                );
            }
            "--color" | "--colour" => {
                let v = value("a colour like #38B6FF")?;
                parse_color(&v).ok_or_else(|| format!("--color: expected #RRGGBB, got {v:?}"))?;
                cli.overrides.color = Some(v);
            }
            "--hotkey" => cli.overrides.hotkey = Some(value("a shortcut")?),
            // macOS adds this when an app is opened from Finder.
            s if s.starts_with("-psn_") => {}
            _ => return Err(format!("unknown option {arg:?}")),
        }
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Cli, String> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_values_in_both_forms() {
        let cli = parse_str("--intensity 450 --blur=80 --lag auto --style rainbow --no-tray").unwrap();
        let mut s = Settings::default();
        cli.overrides.apply(&mut s);
        assert_eq!((s.intensity, s.blur_ms, s.lag_ms, s.style), (450, 80, Lag::Auto, Style::Rainbow));
        assert!(cli.no_tray);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_str("--intensity lots").is_err());
        assert!(parse_str("--color red").is_err());
        assert!(parse_str("--frobnicate").is_err());
        assert!(parse_str("--blur").is_err());
    }
}
