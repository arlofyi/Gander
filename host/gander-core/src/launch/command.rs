// SPDX-License-Identifier: Apache-2.0
//! Building the command a Stream launch runs (`docs/specs/2026-11-01-stream-launch-policy.md`,
//! "Launch and window discovery", step 2).
//!
//! Rules that hold for every [`LaunchCommand`] built here:
//!
//! - The program is always the policy-checked handler executable (`HandlerInfo::exe`). A launch
//!   template must start with `{exe}`; the program named in a registered command string is
//!   ignored and only its arguments are used. So a template or a registry entry can never make
//!   the launch run a different binary than the one the policy allowed.
//! - The file is passed as exactly one argument (one argv element), never through a shell, never
//!   split. The file path must be absolute and free of `"`, NUL and line breaks (none of which a
//!   Windows file name can contain), so it cannot be read as an option or break the quoting.
//! - [`LaunchCommand::command_line`] quotes every argument with the rules `CommandLineToArgvW` and
//!   the Microsoft C runtime use to split it again, so the launched app sees the same argv.

use std::path::Path;

use crate::policy::IsolatedBrowserLaunch;

/// A program and its arguments, one element per argument (nothing pre-joined).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    /// The executable (always the policy-checked handler). Passed to `CreateProcessW` as
    /// `lpApplicationName`, so no search path is involved.
    pub program: String,
    /// Arguments after the program.
    pub args: Vec<String>,
}

/// Why no launch command could be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError {
    /// The template does not start with `{exe}`: it would run another program.
    #[error("the launch template must start with {{exe}}")]
    TemplateProgram,
    /// The template has no `{file}` placeholder.
    #[error("the launch template must contain {{file}}")]
    TemplateNoFile,
    /// The template has a `{...}` that is not `{exe}` or `{file}`.
    #[error("the launch template has an unknown placeholder")]
    TemplatePlaceholder,
    /// The template is empty.
    #[error("the launch template is empty")]
    TemplateEmpty,
    /// The file path is not absolute or contains a character no file name can contain.
    #[error("the file path cannot be passed as one argument")]
    UnsafeFile,
    /// The handler path is not an absolute executable path (for example a packaged app's
    /// `shell:AppsFolder\...`); those are activated, not started.
    #[error("the handler is not an executable path")]
    NotAnExecutable,
}

impl LaunchCommand {
    /// The full command line for `CreateProcessW` (`lpCommandLine`): the program quoted, then
    /// each argument quoted when needed.
    pub fn command_line(&self) -> String {
        let mut out = String::with_capacity(self.program.len() + 16);
        // argv[0] is split by "up to the next quote", without backslash escapes; a Windows path
        // cannot contain a quote, so plain quoting is exact.
        out.push('"');
        out.push_str(&self.program);
        out.push('"');
        for arg in &self.args {
            out.push(' ');
            quote_arg(arg, &mut out);
        }
        out
    }
}

/// Appends `arg` quoted for `CommandLineToArgvW` / the MSVC runtime: unchanged when it has no
/// space, tab, line break or quote and is not empty; otherwise wrapped in quotes with
/// backslashes doubled before a quote (and at the end) and quotes escaped.
pub fn quote_arg(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '"'));
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// Splits arguments the way `CommandLineToArgvW` splits everything after argv[0]: whitespace
/// separates, quotes group, `2n` backslashes before a quote give `n` backslashes and toggle the
/// quote, `2n+1` give `n` backslashes and a literal quote, `""` inside quotes is a literal quote.
pub fn split_args(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        while i < chars.len() && matches!(chars[i], ' ' | '\t' | '\n' | '\r') {
            i += 1;
        }
        if i >= chars.len() {
            return out;
        }
        let mut cur = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let c = chars[i];
            if !in_quotes && matches!(c, ' ' | '\t' | '\n' | '\r') {
                break;
            }
            if c == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                if i < chars.len() && chars[i] == '"' {
                    cur.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.extend(std::iter::repeat_n('\\', n));
                }
                continue;
            }
            if c == '"' {
                if in_quotes && chars.get(i + 1) == Some(&'"') {
                    cur.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = !in_quotes;
                i += 1;
                continue;
            }
            cur.push(c);
            i += 1;
        }
        out.push(cur);
    }
}

/// Removes a verbatim prefix: `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\s` -> `\\srv\s`. Apps launched
/// with a file argument do not all understand verbatim paths.
pub fn strip_verbatim(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return rest.to_owned();
        }
    }
    path.to_owned()
}

/// A Windows absolute path (`C:\...` or `\\server\share\...`, verbatim prefix already removed)
/// that can travel as a single argument.
fn checked_windows_path(path: &Path) -> Result<String, CommandError> {
    let raw = path.to_str().ok_or(CommandError::UnsafeFile)?;
    let s = strip_verbatim(raw);
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let unc = b.len() >= 3 && b[0] == b'\\' && b[1] == b'\\' && b[2] != b'\\' && b[2] != b'?';
    if !(drive || unc) {
        return Err(CommandError::UnsafeFile);
    }
    if s.chars().any(|c| matches!(c, '"' | '\0' | '\n' | '\r')) {
        return Err(CommandError::UnsafeFile);
    }
    Ok(s)
}

fn checked_file(file: &Path) -> Result<String, CommandError> {
    checked_windows_path(file)
}

fn checked_program(exe: &Path) -> Result<String, CommandError> {
    checked_windows_path(exe).map_err(|_| CommandError::NotAnExecutable)
}

/// Builds the command for a catalogue template such as `{exe} --new-window {file}`.
///
/// The template is split like a command line (quotes allowed, they group words); the first token
/// must be exactly `{exe}`. In every later token `{file}` and `{exe}` are substituted in place, so
/// `--app={file}` stays one argument. A substituted value is never split or re-scanned.
pub fn from_template(
    template: &str,
    exe: &Path,
    file: &Path,
) -> Result<LaunchCommand, CommandError> {
    let program = checked_program(exe)?;
    let file = checked_file(file)?;
    let tokens = split_args(template);
    let (first, rest) = tokens.split_first().ok_or(CommandError::TemplateEmpty)?;
    if first != "{exe}" {
        return Err(CommandError::TemplateProgram);
    }
    let mut saw_file = false;
    let mut args = Vec::with_capacity(rest.len());
    for token in rest {
        let mut out = String::with_capacity(token.len());
        let mut tail = token.as_str();
        while let Some(start) = tail.find('{') {
            out.push_str(&tail[..start]);
            let at = &tail[start..];
            if let Some(after) = at.strip_prefix("{file}") {
                out.push_str(&file);
                saw_file = true;
                tail = after;
            } else if let Some(after) = at.strip_prefix("{exe}") {
                out.push_str(&program);
                tail = after;
            } else {
                return Err(CommandError::TemplatePlaceholder);
            }
        }
        out.push_str(tail);
        args.push(out);
    }
    if !saw_file {
        return Err(CommandError::TemplateNoFile);
    }
    Ok(LaunchCommand { program, args })
}

/// The arguments part of a registered command string: everything after the program token.
///
/// The program is quoted (`"C:\...\app.exe" ...`) or, unquoted, ends at the first `.exe`
/// followed by whitespace (registry entries such as `C:\Program Files\x\app.exe %1` exist) or at
/// the first whitespace.
fn registered_args(command: &str) -> &str {
    let s = command.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        return rest.find('"').map_or("", |end| &rest[end + 1..]);
    }
    let lower = s.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find(".exe") {
        let end = search + pos + 4;
        match s[end..].chars().next() {
            None => return "",
            Some(c) if c.is_whitespace() => return &s[end..],
            _ => search = end,
        }
    }
    s.find(char::is_whitespace).map_or("", |end| &s[end..])
}

/// What one `%x` sequence in a registered command means.
enum Percent {
    /// `%1`, `%L`, `%l`, `%V`, `%v`: the file.
    File,
    /// `%%`: a literal percent.
    Literal,
    /// `%*`, `%I`, `%2`..`%9`, `%D`, `%W`, `%U`, `%S`, `%H`, ...: something only the shell can
    /// fill; the whole argument is dropped.
    Unsupported,
}

fn classify(c: char) -> Percent {
    match c {
        '1' | 'L' | 'l' | 'V' | 'v' => Percent::File,
        '%' => Percent::Literal,
        _ => Percent::Unsupported,
    }
}

/// Builds the command for `launch = "default"` from the handler's registered command string
/// (`ASSOCSTR_COMMAND`), keeping the handler executable as the program.
///
/// In the arguments `%1`, `%L` and `%V` become the file (in place, so `/open=%1` stays one
/// argument); an argument holding any other shell placeholder (`%*`, `%I`, `%2`, ...) is
/// dropped. Environment-variable style text such as `%APPDATA%` is passed unchanged. When no
/// argument received the file (or there is no command), the file is appended as the last
/// argument.
pub fn from_registered(
    command: Option<&str>,
    exe: &Path,
    file: &Path,
) -> Result<LaunchCommand, CommandError> {
    let program = checked_program(exe)?;
    let file = checked_file(file)?;
    let mut args = Vec::new();
    let mut saw_file = false;
    for token in split_args(command.map_or("", registered_args)) {
        let mut out = String::with_capacity(token.len());
        let mut chars = token.chars().peekable();
        let mut keep = true;
        let mut has_file = false;
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.peek().copied() {
                // A letter followed by more name characters starts an environment-variable
                // reference such as `%APPDATA%`, copied unchanged up to its closing `%`.
                Some(next) => {
                    let after = {
                        let mut look = chars.clone();
                        look.next();
                        look.next()
                    };
                    let is_env_name = next.is_ascii_alphabetic()
                        && after.is_some_and(|a| a.is_ascii_alphanumeric() || a == '_');
                    if is_env_name {
                        out.push('%');
                        while let Some(&n) = chars.peek() {
                            if n.is_ascii_alphanumeric() || n == '_' || n == '(' || n == ')' {
                                out.push(n);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        if chars.peek() == Some(&'%') {
                            out.push('%');
                            chars.next();
                        }
                        continue;
                    }
                    chars.next();
                    match classify(next) {
                        Percent::File => {
                            out.push_str(&file);
                            has_file = true;
                        }
                        Percent::Literal => out.push('%'),
                        Percent::Unsupported => keep = false,
                    }
                }
                None => out.push('%'),
            }
        }
        if keep {
            saw_file |= has_file;
            args.push(out);
        }
    }
    if !saw_file {
        args.push(file);
    }
    Ok(LaunchCommand { program, args })
}

/// Builds the isolated browser window command (policy step 6): the browser executable and
/// [`IsolatedBrowserLaunch::args`] with the file as a `file:` URL.
pub fn for_browser(
    launch: &IsolatedBrowserLaunch,
    exe: &Path,
    file: &Path,
    profile_dir: &Path,
) -> Result<LaunchCommand, CommandError> {
    let program = checked_program(exe)?;
    let file = checked_file(file)?;
    let profile = checked_windows_path(profile_dir).map_err(|_| CommandError::UnsafeFile)?;
    Ok(LaunchCommand {
        program,
        args: launch.args(&file_url(&file), Path::new(&profile)),
    })
}

/// A `file:` URL for an absolute Windows path: `C:\a b\x.pdf` -> `file:///C:/a%20b/x.pdf`,
/// `\\srv\share\x.pdf` -> `file://srv/share/x.pdf`. Every byte outside the unreserved set (and
/// `/`, and the drive colon) is percent-encoded, UTF-8 first.
pub fn file_url(windows_path: &str) -> String {
    let path = strip_verbatim(windows_path);
    let (prefix, rest) = match path.strip_prefix(r"\\") {
        Some(unc) => ("file://", unc.to_owned()),
        None => ("file:///", path),
    };
    let mut out = String::from(prefix);
    let bytes = rest.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\\' | b'/' => out.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            b':' if prefix == "file:///" && i == 1 => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Browser;

    const EXE: &str = r"C:\Program Files\App\app.exe";
    const FILE: &str = r"C:\Shares\Work\Deck One.pptx";

    fn tpl(t: &str) -> Result<LaunchCommand, CommandError> {
        from_template(t, Path::new(EXE), Path::new(FILE))
    }

    fn reg(c: &str) -> LaunchCommand {
        from_registered(Some(c), Path::new(EXE), Path::new(FILE)).unwrap()
    }

    #[test]
    fn template_basic() {
        let c = tpl("{exe} --new-window {file}").unwrap();
        assert_eq!(c.program, EXE);
        assert_eq!(c.args, vec!["--new-window", FILE]);
        assert_eq!(
            c.command_line(),
            r#""C:\Program Files\App\app.exe" --new-window "C:\Shares\Work\Deck One.pptx""#
        );
    }

    #[test]
    fn template_embedded_and_quoted() {
        let c = tpl(r#""{exe}" "--open={file}" -x"#).unwrap();
        assert_eq!(c.args, vec![format!("--open={FILE}"), "-x".to_owned()]);
    }

    #[test]
    fn template_refusals() {
        assert_eq!(
            tpl(r"C:\Windows\System32\cmd.exe /c {file}"),
            Err(CommandError::TemplateProgram)
        );
        assert_eq!(tpl("{file} {exe}"), Err(CommandError::TemplateProgram));
        assert_eq!(tpl("{exe} --x"), Err(CommandError::TemplateNoFile));
        assert_eq!(tpl("{exe} {path}"), Err(CommandError::TemplatePlaceholder));
        assert_eq!(tpl("   "), Err(CommandError::TemplateEmpty));
    }

    #[test]
    fn file_must_be_one_safe_absolute_argument() {
        for bad in [
            "Deck.pptx",
            r"..\x.pptx",
            r"\x.pptx",
            "C:x.pptx",
            "C:\\a\"b.pptx",
            "C:\\a\nb.pptx",
        ] {
            assert_eq!(
                from_template("{exe} {file}", Path::new(EXE), Path::new(bad)),
                Err(CommandError::UnsafeFile),
                "{bad}"
            );
        }
        let c = from_template(
            "{exe} {file}",
            Path::new(EXE),
            Path::new(r"\\?\C:\a b\x.txt"),
        )
        .unwrap();
        assert_eq!(c.args, vec![r"C:\a b\x.txt"]);
        let c = from_template(
            "{exe} {file}",
            Path::new(EXE),
            Path::new(r"\\?\UNC\srv\s\x.txt"),
        )
        .unwrap();
        assert_eq!(c.args, vec![r"\\srv\s\x.txt"]);
    }

    #[test]
    fn packaged_handler_is_not_an_executable() {
        let r = from_template(
            "{exe} {file}",
            Path::new(r"shell:AppsFolder\Microsoft.Windows.Photos_8wekyb3d8bbwe!App"),
            Path::new(FILE),
        );
        assert_eq!(r, Err(CommandError::NotAnExecutable));
    }

    #[test]
    fn a_file_name_that_looks_like_an_option_stays_a_path() {
        let c = tpl("{exe} {file}").unwrap();
        let odd = r"C:\s\--help --evil.txt";
        let c2 = from_template("{exe} {file}", Path::new(EXE), Path::new(odd)).unwrap();
        assert_eq!(split_args(&c2.command_line())[1], odd);
        assert_eq!(split_args(&c.command_line())[1], FILE);
    }

    #[test]
    fn registered_commands() {
        assert_eq!(
            reg(r#""C:\Other\POWERPNT.EXE" /n "%1""#).args,
            vec!["/n", FILE]
        );
        // The registered program is ignored: the policy-checked exe is the program.
        assert_eq!(reg(r#""C:\Other\POWERPNT.EXE" /n "%1""#).program, EXE);
        assert_eq!(reg(r"C:\Program Files\x\app.exe %1").args, vec![FILE]);
        assert_eq!(
            reg(r"C:\x\app.exe /open=%L").args,
            vec![format!("/open={FILE}")]
        );
        assert_eq!(reg(r#""C:\x\app.exe" "%1" %*"#).args, vec![FILE]);
        assert_eq!(reg(r#""C:\x\app.exe" /idlist,%I,%L"#).args, vec![FILE]);
        assert_eq!(
            reg(r#""C:\x\app.exe" --profile %APPDATA%\x "%1""#).args,
            vec!["--profile", r"%APPDATA%\x", FILE]
        );
        assert_eq!(reg(r#""C:\x\app.exe" 100%% "%1""#).args, vec!["100%", FILE]);
        // No placeholder: the file is appended.
        assert_eq!(reg(r#""C:\x\app.exe" -n"#).args, vec!["-n", FILE]);
        assert_eq!(
            from_registered(None, Path::new(EXE), Path::new(FILE))
                .unwrap()
                .args,
            vec![FILE]
        );
    }

    #[test]
    fn a_percent_in_the_file_name_is_not_rescanned() {
        let f = r"C:\s\100%1 done.txt";
        let c =
            from_registered(Some(r#""C:\x\a.exe" "%1""#), Path::new(EXE), Path::new(f)).unwrap();
        assert_eq!(c.args, vec![f]);
        let c =
            from_template("{exe} {file}", Path::new(EXE), Path::new(r"C:\s\{exe}.txt")).unwrap();
        assert_eq!(c.args, vec![r"C:\s\{exe}.txt"]);
    }

    #[test]
    fn quoting_round_trips() {
        let cases = [
            "plain",
            "",
            "with space",
            r"trailing\",
            r"trail space\ ",
            r#"quote"inside"#,
            r#"back\"slash"#,
            r"C:\a b\",
            r"\\srv\share\x y",
            "tab\there",
        ];
        for arg in cases {
            let mut s = String::new();
            quote_arg(arg, &mut s);
            assert_eq!(
                split_args(&s),
                vec![arg.to_owned()],
                "arg {arg:?} quoted as {s}"
            );
        }
        let c = LaunchCommand {
            program: EXE.into(),
            args: cases.iter().map(|s| (*s).to_owned()).collect(),
        };
        let line = c.command_line();
        let rest = line.strip_prefix(&format!("\"{EXE}\" ")).unwrap();
        assert_eq!(split_args(rest), c.args);
    }

    #[test]
    fn split_args_rules() {
        assert_eq!(split_args(r#"a "b c" d"#), vec!["a", "b c", "d"]);
        assert_eq!(split_args(r#"a\\\"b"#), vec![r#"a\"b"#]);
        assert_eq!(split_args(r#""a""b""#), vec![r#"a"b"#]);
        assert_eq!(split_args(r"a\b\\ c"), vec![r"a\b\\", "c"]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url(r"C:\s\a b.pdf"), "file:///C:/s/a%20b.pdf");
        assert_eq!(
            file_url(r"\\srv\share\x#1.pdf"),
            "file://srv/share/x%231.pdf"
        );
        assert_eq!(
            file_url("C:\\s\\caf\u{e9}.pdf"),
            "file:///C:/s/caf%C3%A9.pdf"
        );
        assert_eq!(file_url(r"\\?\C:\s\x.pdf"), "file:///C:/s/x.pdf");
        assert_eq!(file_url(r"C:\s\a:b%.pdf"), "file:///C:/s/a%3Ab%25.pdf");
    }

    #[test]
    fn browser_templates_by_argv() {
        let edge = IsolatedBrowserLaunch {
            browser: Browser::Edge,
        };
        let c = for_browser(
            &edge,
            Path::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
            Path::new(r"C:\s\Manual 2.pdf"),
            Path::new(r"C:\Users\u\AppData\Local\Gander\stream-browser\edge"),
        )
        .unwrap();
        assert_eq!(
            c.args,
            vec![
                "--new-window",
                "--app=file:///C:/s/Manual%202.pdf",
                r"--user-data-dir=C:\Users\u\AppData\Local\Gander\stream-browser\edge",
            ]
        );
        let line = c.command_line();
        assert!(line.starts_with(
            r#""C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" --new-window"#
        ));
        let ff = IsolatedBrowserLaunch {
            browser: Browser::Firefox,
        };
        let c = for_browser(
            &ff,
            Path::new(r"C:\Program Files\Mozilla Firefox\firefox.exe"),
            Path::new(r"C:\s\a.pdf"),
            Path::new(r"C:\p"),
        )
        .unwrap();
        assert_eq!(
            c.args,
            vec!["-new-window", "-P", "gander-stream", "file:///C:/s/a.pdf"]
        );
        assert_eq!(
            for_browser(
                &edge,
                Path::new(r"C:\b\msedge.exe"),
                Path::new(r"C:\s\a.pdf"),
                Path::new("relative")
            ),
            Err(CommandError::UnsafeFile)
        );
    }
}
