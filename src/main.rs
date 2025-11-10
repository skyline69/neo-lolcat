use std::{
    borrow::Cow,
    env,
    fs::File,
    io::{self, BufReader, IsTerminal, Read, Write},
    process, thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RESET: &str = "\x1b[0m";
const RESET_FG: &str = "\x1b[39m";
const RESET_BG: &str = "\x1b[49m";
const SAVE_CURSOR: &str = "\x1b7";
const RESTORE_CURSOR: &str = "\x1b8";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const READ_CHUNK: usize = 64 * 1024;
const PENDING_CAP: usize = 4096;
const SHIFT_COS: f64 = -0.5;
const SHIFT_SIN: f64 = 0.866_025_403_784_438_6;

const HELP_TEXT: &str = r#"Usage: lolcat [OPTION]... [FILE]...

Concatenate FILE(s), or standard input, to standard output.
With no FILE, or when FILE is -, read standard input.

  -p, --spread=<f>      Rainbow spread (default: 3.0)
  -F, --freq=<f>        Rainbow frequency (default: 0.1)
  -S, --seed=<i>        Rainbow seed, 0 = random (default: 0)
  -a, --animate         Enable psychedelics
  -d, --duration=<i>    Animation duration (default: 12)
  -s, --speed=<f>       Animation speed (default: 20.0)
  -i, --invert          Invert fg and bg
  -t, --truecolor       24-bit (truecolor)
  -f, --force           Force color even when stdout is not a tty
  -D, --debug           Print internal diagnostics
  -v, --version         Print version and exit
  -h, --help            Show this message

Examples:
  lolcat f - g      Output f's contents, then stdin, then g's contents.
  lolcat            Copy standard input to standard output.
  fortune | lolcat  Display a rainbow cookie.

Report neo-lolcat bugs to <https://github.com/skyline69/neo-lolcat/issues>
neo-lolcat home page: <https://github.com/skyline69/neo-lolcat/>
Report lolcat translation bugs to <http://speaklolcat.com/>
"#;

fn main() {
    process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    let config = match Config::parse(&args) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("lolcat: {err}");
            return 1;
        }
    };

    if config.version {
        println!("neo-lolcat {} (c)2025 Ö. Efe D.", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    if config.help {
        if let Err(err) = print_help(&config) {
            eprintln!("lolcat: failed to render help: {err}");
            return 1;
        }
        return 0;
    }

    match execute(&config) {
        RunStatus::Success => 0,
        RunStatus::Reported => 1,
        RunStatus::BrokenPipe => 0,
        RunStatus::Io(err) => {
            eprintln!("lolcat: {err}");
            1
        }
    }
}

fn debug_log(cfg: &Config, msg: &str) {
    if cfg.debug {
        eprintln!("[lolcat] {msg}");
    }
}

fn print_help(config: &Config) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut help_cfg = config.clone();
    help_cfg.force = true;
    help_cfg.animate = false;
    help_cfg.spread = 8.0;
    help_cfg.freq = 0.3;
    let color_mode = choose_color_mode(&help_cfg);
    let mut printer = Printer::new(&help_cfg, true, color_mode, random_seed_offset(8192.0));
    printer.print_text(HELP_TEXT, &mut handle)?;
    match printer.finalize(&mut handle) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(err),
    }
}

fn execute(config: &Config) -> RunStatus {
    let stdout = io::stdout();
    let stdout_is_tty = stdout.is_terminal();
    let mut handle = stdout.lock();
    let use_color = stdout_is_tty || config.force;
    let color_mode = if use_color {
        choose_color_mode(config)
    } else {
        ColorMode::Ansi256
    };
    debug_log(
        config,
        &format!(
            "use_color={}, mode={:?}, animate={}, spread={}, freq={}",
            use_color, color_mode, config.animate, config.spread, config.freq
        ),
    );
    let mut printer = Printer::new(config, use_color, color_mode, initial_offset(config.seed));

    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();
    let files: Vec<String> = if config.files.is_empty() {
        vec!["-".to_string()]
    } else {
        config.files.clone()
    };

    for path in files {
        debug_log(config, &format!("processing source '{path}'"));
        let result = if path == "-" {
            process_stream(&mut stdin_lock, &mut handle, &mut printer)
        } else {
            match File::open(&path) {
                Ok(file) => process_stream(file, &mut handle, &mut printer),
                Err(err) => {
                    eprintln!("{}", describe_error(&path, &err));
                    let _ = printer.finalize(&mut handle);
                    return RunStatus::Reported;
                }
            }
        };

        match result {
            Ok(()) => {}
            Err(StreamError::BrokenPipe) => return RunStatus::BrokenPipe,
            Err(StreamError::Io(err)) => {
                let _ = printer.finalize(&mut handle);
                return RunStatus::Io(err);
            }
        }
    }

    match printer.finalize(&mut handle) {
        Ok(()) => RunStatus::Success,
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => RunStatus::BrokenPipe,
        Err(err) => RunStatus::Io(err),
    }
}

fn process_stream<R: Read>(
    reader: R,
    writer: &mut dyn Write,
    printer: &mut Printer,
) -> Result<(), StreamError> {
    if !printer.use_color {
        let mut reader = BufReader::new(reader);
        io::copy(&mut reader, writer).map_err(StreamError::from)?;
        return Ok(());
    }

    if printer.cfg.animate {
        process_stream_buffered(reader, writer, printer)
    } else {
        process_stream_streaming(reader, writer, printer)
    }
}

fn process_stream_buffered<R: Read>(
    reader: R,
    writer: &mut dyn Write,
    printer: &mut Printer,
) -> Result<(), StreamError> {
    let mut reader = BufReader::new(reader);
    let mut chunk = [0u8; READ_CHUNK];
    let mut line_buf = Vec::new();
    loop {
        let read = reader.read(&mut chunk).map_err(StreamError::from)?;
        if read == 0 {
            if !line_buf.is_empty() {
                flush_line(&mut line_buf, false, printer, writer)?;
            }
            break;
        }

        let mut start = 0;
        for (idx, &byte) in chunk[..read].iter().enumerate() {
            if byte == b'\n' {
                line_buf.extend_from_slice(&chunk[start..idx]);
                flush_line(&mut line_buf, true, printer, writer)?;
                start = idx + 1;
            }
        }

        if start < read {
            line_buf.extend_from_slice(&chunk[start..read]);
        }
    }
    printer.flush_pending(writer).map_err(StreamError::from)
}

fn process_stream_streaming<R: Read>(
    mut reader: R,
    writer: &mut dyn Write,
    printer: &mut Printer,
) -> Result<(), StreamError> {
    let mut buffer = [0u8; READ_CHUNK + 4];
    let mut carry = 0usize;

    'outer: loop {
        let read = reader
            .read(&mut buffer[carry..])
            .map_err(StreamError::from)?;
        if read == 0 {
            break;
        }
        let total = carry + read;
        let mut offset = 0usize;

        while offset < total {
            match std::str::from_utf8(&buffer[offset..total]) {
                Ok(valid) => {
                    consume_segment(valid, printer, writer).map_err(StreamError::from)?;
                    offset = total;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        let slice = std::str::from_utf8(&buffer[offset..offset + valid_up_to])
                            .expect("validator provided a valid prefix");
                        consume_segment(slice, printer, writer).map_err(StreamError::from)?;
                        offset += valid_up_to;
                        continue;
                    }
                    if let Some(error_len) = err.error_len() {
                        printer
                            .write_replacement(writer)
                            .map_err(StreamError::from)?;
                        offset += error_len;
                        continue;
                    }
                    carry = total - offset;
                    buffer.copy_within(offset..total, 0);
                    continue 'outer;
                }
            }
        }
        carry = 0;
    }

    if carry > 0 {
        printer
            .write_replacement(writer)
            .map_err(StreamError::from)?;
    }

    printer.flush_pending(writer).map_err(StreamError::from)
}

fn consume_segment(
    segment: &str,
    printer: &mut Printer<'_>,
    writer: &mut dyn Write,
) -> io::Result<()> {
    if segment.is_empty() {
        return Ok(());
    }
    let mut start = 0;
    for (idx, ch) in segment.char_indices() {
        if ch == '\n' {
            if idx > start {
                printer.write_plain_segment(&segment[start..idx], writer)?;
            }
            printer.finish_line(writer)?;
            start = idx + ch.len_utf8();
        }
    }
    if start < segment.len() {
        printer.write_plain_segment(&segment[start..], writer)?;
    }
    Ok(())
}

fn flush_line(
    line_buf: &mut Vec<u8>,
    had_newline: bool,
    printer: &mut Printer,
    writer: &mut dyn Write,
) -> Result<(), StreamError> {
    if line_buf.is_empty() && !had_newline {
        return Ok(());
    }
    let text = match String::from_utf8_lossy(line_buf) {
        Cow::Owned(s) => s,
        Cow::Borrowed(s) => s.to_string(),
    };
    printer
        .print_line(&text, had_newline, writer)
        .map_err(StreamError::from)?;
    line_buf.clear();
    Ok(())
}

fn describe_error(path: &str, err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::NotFound => format!("lolcat: {path}: No such file or directory"),
        io::ErrorKind::PermissionDenied => format!("lolcat: {path}: Permission denied"),
        _ => match err.raw_os_error() {
            Some(21) => format!("lolcat: {path}: Is a directory"),
            Some(25) => format!("lolcat: {path}: Inappropriate ioctl for device"),
            Some(6) => format!("lolcat: {path}: Is not a regular file"),
            _ => format!("lolcat: {path}: {err}"),
        },
    }
}

#[derive(Clone, Debug)]
struct Config {
    spread: f64,
    freq: f64,
    seed: u64,
    animate: bool,
    duration: u32,
    speed: f64,
    invert: bool,
    truecolor: bool,
    force: bool,
    debug: bool,
    version: bool,
    help: bool,
    files: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            spread: 3.0,
            freq: 0.1,
            seed: 0,
            animate: false,
            duration: 12,
            speed: 20.0,
            invert: false,
            truecolor: false,
            force: false,
            debug: false,
            version: false,
            help: false,
            files: Vec::new(),
        }
    }
}

impl Config {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut cfg = Config::default();
        let mut iter = args.iter().peekable();
        let mut files = Vec::new();
        while let Some(arg) = iter.next() {
            if arg == "--" {
                files.extend(iter.map(|s| s.to_string()));
                break;
            } else if arg.starts_with("--") {
                Self::parse_long(arg, &mut cfg, &mut iter)?;
            } else if arg.starts_with('-') && arg.len() > 1 {
                Self::parse_short(arg, &mut cfg, &mut iter)?;
            } else {
                files.push(arg.to_string());
            }
        }
        cfg.files = files;
        if !cfg.debug && env::var("LOLCAT_DEBUG").is_ok() {
            cfg.debug = true;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn parse_long<'a, I>(
        arg: &str,
        cfg: &mut Config,
        iter: &mut std::iter::Peekable<I>,
    ) -> Result<(), String>
    where
        I: Iterator<Item = &'a String>,
    {
        let mut parts = arg[2..].splitn(2, '=');
        let name = parts.next().unwrap_or_default();
        let value = parts.next();
        match name {
            "spread" => {
                cfg.spread = Self::parse_f64("spread", value, iter)?;
            }
            "freq" => {
                cfg.freq = Self::parse_f64("freq", value, iter)?;
            }
            "seed" => {
                cfg.seed = Self::parse_u64("seed", value, iter)?;
            }
            "animate" => {
                cfg.animate = true;
                if let Some(val) = value {
                    Self::override_duration(cfg, "animate", val.to_string())?;
                } else if let Some(raw) = Self::consume_numeric_arg(iter) {
                    Self::override_duration(cfg, "animate", raw)?;
                }
            }
            "duration" => {
                let val = Self::parse_f64("duration", value, iter)?;
                cfg.duration = float_duration_to_frames(val)?;
            }
            "speed" => {
                cfg.speed = Self::parse_f64("speed", value, iter)?;
            }
            "invert" => cfg.invert = true,
            "truecolor" => cfg.truecolor = true,
            "force" => cfg.force = true,
            "debug" => cfg.debug = true,
            "version" => cfg.version = true,
            "help" => cfg.help = true,
            _ => {
                if !name.is_empty() {
                    return Err(format!("unknown option '--{name}'"));
                }
            }
        }
        Ok(())
    }

    fn parse_short<'a, I>(
        arg: &str,
        cfg: &mut Config,
        iter: &mut std::iter::Peekable<I>,
    ) -> Result<(), String>
    where
        I: Iterator<Item = &'a String>,
    {
        let mut chars = arg[1..].chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                'p' => {
                    let value = Self::attached_value(&mut chars, iter, "-p")?;
                    cfg.spread = parse_f64_value("spread", value)?;
                    break;
                }
                'F' => {
                    let value = Self::attached_value(&mut chars, iter, "-F")?;
                    cfg.freq = parse_f64_value("freq", value)?;
                    break;
                }
                'S' => {
                    let value = Self::attached_value(&mut chars, iter, "-S")?;
                    cfg.seed = parse_u64_value("seed", value)?;
                    break;
                }
                'd' => {
                    let value = Self::attached_value(&mut chars, iter, "-d")?;
                    cfg.duration = float_duration_to_frames(parse_f64_value("duration", value)?)?;
                    break;
                }
                's' => {
                    let value = Self::attached_value(&mut chars, iter, "-s")?;
                    cfg.speed = parse_f64_value("speed", value)?;
                    break;
                }
                'a' => {
                    cfg.animate = true;
                    if let Some(raw) = Self::consume_numeric_arg(iter) {
                        Self::override_duration(cfg, "animate", raw)?;
                    }
                }
                'i' => cfg.invert = true,
                't' => cfg.truecolor = true,
                'f' => cfg.force = true,
                'D' => cfg.debug = true,
                'v' => cfg.version = true,
                'h' => cfg.help = true,
                other => {
                    return Err(format!("unknown option '-{other}'"));
                }
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.spread < 0.1 {
            return Err("--spread must be >= 0.1".to_string());
        }
        if self.speed < 0.1 {
            return Err("--speed must be >= 0.1".to_string());
        }
        if self.duration == 0 {
            return Err("--duration must be >= 1".to_string());
        }
        Ok(())
    }

    fn parse_f64<'a, I>(
        name: &str,
        value: Option<&str>,
        iter: &mut std::iter::Peekable<I>,
    ) -> Result<f64, String>
    where
        I: Iterator<Item = &'a String>,
    {
        if let Some(val) = value {
            return parse_f64_value(name, val.to_string());
        }
        let next = iter
            .next()
            .ok_or_else(|| format!("--{name} requires a value"))?;
        parse_f64_value(name, next.to_string())
    }

    fn parse_u64<'a, I>(
        name: &str,
        value: Option<&str>,
        iter: &mut std::iter::Peekable<I>,
    ) -> Result<u64, String>
    where
        I: Iterator<Item = &'a String>,
    {
        if let Some(val) = value {
            return parse_u64_value(name, val.to_string());
        }
        let next = iter
            .next()
            .ok_or_else(|| format!("--{name} requires a value"))?;
        parse_u64_value(name, next.to_string())
    }

    fn attached_value<'a, I>(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        iter: &mut std::iter::Peekable<I>,
        flag: &str,
    ) -> Result<String, String>
    where
        I: Iterator<Item = &'a String>,
    {
        let collected: String = chars.collect();
        if !collected.is_empty() {
            Ok(collected)
        } else {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value"))
        }
    }

    fn override_duration(cfg: &mut Config, flag: &str, raw: String) -> Result<(), String> {
        let val = parse_f64_value(flag, raw)?;
        cfg.duration = float_duration_to_frames(val)?;
        Ok(())
    }

    fn consume_numeric_arg<'a, I>(iter: &mut std::iter::Peekable<I>) -> Option<String>
    where
        I: Iterator<Item = &'a String>,
    {
        if let Some(next) = iter.peek()
            && next.parse::<f64>().is_ok()
        {
            return iter.next().cloned();
        }
        None
    }
}

fn float_duration_to_frames(value: f64) -> Result<u32, String> {
    if value < 0.1 {
        Err("--duration must be >= 0.1".to_string())
    } else {
        Ok(value.round().max(1.0) as u32)
    }
}

fn parse_f64_value(name: &str, value: String) -> Result<f64, String> {
    value
        .parse::<f64>()
        .map_err(|_| format!("invalid value for --{name}: '{value}'"))
}

fn parse_u64_value(name: &str, value: String) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid value for --{name}: '{value}'"))
}

enum RunStatus {
    Success,
    Reported,
    BrokenPipe,
    Io(io::Error),
}

#[derive(Debug)]
enum StreamError {
    BrokenPipe,
    Io(io::Error),
}

impl From<io::Error> for StreamError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::BrokenPipe {
            StreamError::BrokenPipe
        } else {
            StreamError::Io(err)
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum ColorMode {
    TrueColor,
    Ansi256,
}

fn choose_color_mode(config: &Config) -> ColorMode {
    let env_term = env::var("COLORTERM").ok();
    choose_color_mode_from(config, env_term.as_deref())
}

fn choose_color_mode_from(config: &Config, env_term: Option<&str>) -> ColorMode {
    if config.truecolor || detects_truecolor_from(env_term) {
        ColorMode::TrueColor
    } else {
        ColorMode::Ansi256
    }
}

fn detects_truecolor_from(term: Option<&str>) -> bool {
    term.map(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("truecolor") || lower.contains("24bit")
    })
    .unwrap_or(false)
}

fn initial_offset(seed: u64) -> f64 {
    if seed == 0 {
        random_seed_offset(256.0)
    } else {
        (seed % 256) as f64
    }
}

fn random_seed_offset(range: f64) -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| (dur.as_nanos() % (range as u128)) as f64)
        .unwrap_or(0.0)
}

struct Printer<'a> {
    cfg: &'a Config,
    os: f64,
    use_color: bool,
    color_mode: ColorMode,
    cursor_hidden: bool,
    line_active: bool,
    escape_state: EscapeState,
    phase: RainbowState,
    rot: RainbowRot,
    buffer: SmallBuf,
}

impl<'a> Printer<'a> {
    fn new(cfg: &'a Config, use_color: bool, color_mode: ColorMode, offset: f64) -> Self {
        let angle = cfg.freq * offset;
        Self {
            cfg,
            os: offset,
            use_color,
            color_mode,
            cursor_hidden: false,
            line_active: false,
            escape_state: EscapeState::Idle,
            phase: RainbowState::from_angle(angle),
            rot: RainbowRot::new(cfg.freq / cfg.spread),
            buffer: SmallBuf::new(),
        }
    }

    fn finalize(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        if self.cursor_hidden {
            self.buffer.push(writer, SHOW_CURSOR.as_bytes())?;
            self.cursor_hidden = false;
        }
        if self.use_color {
            self.buffer.push(writer, RESET.as_bytes())?;
        }
        self.buffer.flush(writer)?;
        writer.flush()
    }

    fn print_text(&mut self, text: &str, writer: &mut dyn Write) -> io::Result<()> {
        for line in text.split_inclusive('\n') {
            let (body, newline) = if let Some(stripped) = line.strip_suffix('\n') {
                (stripped, true)
            } else {
                (line, false)
            };
            self.print_line(body, newline, writer)?;
        }
        Ok(())
    }

    fn print_line(
        &mut self,
        text: &str,
        had_newline: bool,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        if self.cfg.animate && !text.is_empty() {
            self.animate_line(text, had_newline, writer)
        } else {
            self.print_plain_line(text, had_newline, writer)
        }
    }

    fn animate_line(
        &mut self,
        text: &str,
        had_newline: bool,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        if !self.cursor_hidden {
            self.buffer.push(writer, HIDE_CURSOR.as_bytes())?;
            self.cursor_hidden = true;
        }
        self.buffer.push(writer, SAVE_CURSOR.as_bytes())?;
        let original = self.os;
        let frames = self.cfg.duration;
        let frame_time = Duration::from_secs_f64(1.0 / self.cfg.speed);
        let mut next_frame = Instant::now();
        for _ in 0..frames {
            self.buffer.push(writer, RESTORE_CURSOR.as_bytes())?;
            self.os += self.cfg.spread;
            self.print_plain_line(text, false, writer)?;
            self.buffer.flush(writer)?;
            writer.flush()?;
            next_frame += frame_time;
            let now = Instant::now();
            if next_frame > now {
                thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
        self.os = original;
        if had_newline {
            self.buffer.push(writer, b"\n")?;
            self.os += 1.0;
        }
        self.buffer.flush(writer)?;
        Ok(())
    }

    fn print_plain_line(
        &mut self,
        text: &str,
        had_newline: bool,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        if !self.use_color {
            self.buffer.flush(writer)?;
            writer.write_all(text.as_bytes())?;
            if had_newline {
                writer.write_all(b"\n")?;
            }
            return Ok(());
        }

        self.line_active = false;
        self.escape_state = EscapeState::Idle;
        self.write_plain_segment(text, writer)?;
        if had_newline {
            self.finish_line(writer)?;
        } else {
            self.line_active = false;
        }
        self.escape_state = EscapeState::Idle;
        Ok(())
    }

    fn write_plain_segment(&mut self, text: &str, writer: &mut dyn Write) -> io::Result<()> {
        debug_assert!(self.use_color);
        for ch in text.chars() {
            if self.escape_state.is_active() {
                self.feed_escape(ch, writer)?;
                continue;
            }
            if ch == '\x1b' {
                self.begin_escape(writer)?;
                continue;
            }
            if ch == '\t' {
                for _ in 0..8 {
                    self.write_visible_char(' ', writer)?;
                }
                continue;
            }
            self.write_visible_char(ch, writer)?;
        }
        Ok(())
    }

    fn write_visible_char(&mut self, ch: char, writer: &mut dyn Write) -> io::Result<()> {
        self.ensure_line_active();
        let (r, g, b) = self.phase.channels();
        let encoded = &mut [0u8; 4];
        let glyph = ch.encode_utf8(encoded);
        let mut block = [0u8; 64];
        let mut len = match (self.cfg.invert, self.color_mode) {
            (invert, ColorMode::TrueColor) => build_truecolor_prefix(&mut block, invert, r, g, b),
            (invert, ColorMode::Ansi256) => {
                let idx = rgb_to_ansi256(r, g, b);
                build_ansi_prefix(&mut block, invert, idx)
            }
        };
        block[len..len + glyph.len()].copy_from_slice(glyph.as_bytes());
        len += glyph.len();
        let reset = if self.cfg.invert {
            RESET_BG.as_bytes()
        } else {
            RESET_FG.as_bytes()
        };
        block[len..len + reset.len()].copy_from_slice(reset);
        len += reset.len();
        self.buffer.push(writer, &block[..len])?;
        self.phase.advance(self.rot);
        Ok(())
    }

    fn finish_line(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        self.buffer.push(writer, b"\n")?;
        self.os += 1.0;
        self.line_active = false;
        Ok(())
    }

    fn ensure_line_active(&mut self) {
        if !self.line_active {
            self.line_active = true;
            self.phase.reset(self.cfg.freq * self.os);
        }
    }

    fn begin_escape(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        self.buffer.push(writer, b"\x1b")?;
        self.escape_state = EscapeState::Start;
        Ok(())
    }

    fn feed_escape(&mut self, ch: char, writer: &mut dyn Write) -> io::Result<()> {
        let mut buf = [0u8; 4];
        let encoded = ch.encode_utf8(&mut buf);
        self.buffer.push(writer, encoded.as_bytes())?;
        self.escape_state.advance(ch);
        Ok(())
    }

    fn write_replacement(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        self.write_visible_char('\u{FFFD}', writer)
    }

    fn flush_pending(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        self.buffer.flush(writer)
    }
}

fn build_truecolor_prefix(buf: &mut [u8], invert: bool, r: u8, g: u8, b: u8) -> usize {
    let mut len = 0;
    buf[len] = 0x1b;
    len += 1;
    buf[len] = b'[';
    len += 1;
    buf[len] = if invert { b'4' } else { b'3' };
    len += 1;
    buf[len] = b'8';
    len += 1;
    buf[len] = b';';
    len += 1;
    buf[len] = b'2';
    len += 1;
    buf[len] = b';';
    len += 1;
    len += append_decimal_u8(&mut buf[len..], r);
    buf[len] = b';';
    len += 1;
    len += append_decimal_u8(&mut buf[len..], g);
    buf[len] = b';';
    len += 1;
    len += append_decimal_u8(&mut buf[len..], b);
    buf[len] = b'm';
    len + 1
}

fn build_ansi_prefix(buf: &mut [u8], invert: bool, idx: u8) -> usize {
    let mut len = 0;
    buf[len] = 0x1b;
    len += 1;
    buf[len] = b'[';
    len += 1;
    buf[len] = if invert { b'4' } else { b'3' };
    len += 1;
    buf[len] = b'8';
    len += 1;
    buf[len] = b';';
    len += 1;
    buf[len] = b'5';
    len += 1;
    buf[len] = b';';
    len += 1;
    len += append_decimal_u8(&mut buf[len..], idx);
    buf[len] = b'm';
    len + 1
}

fn append_decimal_u8(dst: &mut [u8], value: u8) -> usize {
    debug_assert!(dst.len() >= 3);
    let hundreds = value / 100;
    let tens = (value % 100) / 10;
    let ones = value % 10;
    let mut len = 0;
    if hundreds != 0 {
        dst[len] = b'0' + hundreds;
        len += 1;
        dst[len] = b'0' + tens;
        len += 1;
        dst[len] = b'0' + ones;
        len += 1;
    } else if tens != 0 {
        dst[len] = b'0' + tens;
        len += 1;
        dst[len] = b'0' + ones;
        len += 1;
    } else {
        dst[len] = b'0' + ones;
        len += 1;
    }
    len
}

struct SmallBuf {
    data: [u8; PENDING_CAP],
    len: usize,
}

impl SmallBuf {
    fn new() -> Self {
        Self {
            data: [0u8; PENDING_CAP],
            len: 0,
        }
    }

    fn push(&mut self, writer: &mut dyn Write, chunk: &[u8]) -> io::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        if chunk.len() >= self.data.len() {
            self.flush(writer)?;
            return writer.write_all(chunk);
        }
        if self.len + chunk.len() > self.data.len() {
            self.flush(writer)?;
        }
        self.data[self.len..self.len + chunk.len()].copy_from_slice(chunk);
        self.len += chunk.len();
        Ok(())
    }

    fn flush(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        if self.len > 0 {
            writer.write_all(&self.data[..self.len])?;
            self.len = 0;
        }
        Ok(())
    }
}

#[derive(Copy, Clone)]
enum EscapeState {
    Idle,
    Start,
    Csi,
    Osc { saw_esc: bool },
    StringTerm { saw_esc: bool },
    Fe,
}

impl EscapeState {
    fn is_active(self) -> bool {
        !matches!(self, EscapeState::Idle)
    }

    fn advance(&mut self, ch: char) {
        match self {
            EscapeState::Idle => {}
            EscapeState::Start => {
                *self = match ch {
                    '[' => EscapeState::Csi,
                    ']' => EscapeState::Osc { saw_esc: false },
                    'P' | 'X' | '^' | '_' => EscapeState::StringTerm { saw_esc: false },
                    c if (' '..='/').contains(&c) => EscapeState::Fe,
                    _ => EscapeState::Idle,
                };
            }
            EscapeState::Csi => {
                if ('@'..='~').contains(&ch) {
                    *self = EscapeState::Idle;
                }
            }
            EscapeState::Osc { saw_esc } => {
                if ch == '\u{07}' || (*saw_esc && ch == '\\') {
                    *self = EscapeState::Idle;
                    return;
                }
                *saw_esc = ch == '\x1b';
            }
            EscapeState::StringTerm { saw_esc } => {
                if *saw_esc && ch == '\\' {
                    *self = EscapeState::Idle;
                    return;
                }
                *saw_esc = ch == '\x1b';
            }
            EscapeState::Fe => {
                *self = EscapeState::Idle;
            }
        }
    }
}

#[derive(Copy, Clone)]
struct RainbowState {
    sin: f64,
    cos: f64,
}

impl RainbowState {
    fn from_angle(angle: f64) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self { sin, cos }
    }

    fn reset(&mut self, angle: f64) {
        let (sin, cos) = angle.sin_cos();
        self.sin = sin;
        self.cos = cos;
    }

    fn advance(&mut self, rot: RainbowRot) {
        let sin = self.sin * rot.cos + self.cos * rot.sin;
        let cos = self.cos * rot.cos - self.sin * rot.sin;
        self.sin = sin;
        self.cos = cos;
    }

    fn channels(&self) -> (u8, u8, u8) {
        (
            encode_component(self.sin),
            encode_component(self.sin * SHIFT_COS + self.cos * SHIFT_SIN),
            encode_component(self.sin * SHIFT_COS - self.cos * SHIFT_SIN),
        )
    }
}

#[derive(Copy, Clone)]
struct RainbowRot {
    cos: f64,
    sin: f64,
}

impl RainbowRot {
    fn new(delta: f64) -> Self {
        let (sin, cos) = delta.sin_cos();
        Self { cos, sin }
    }
}

fn encode_component(value: f64) -> u8 {
    value.mul_add(127.0, 128.0).round().clamp(0.0, 255.0) as u8
}

fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            16
        } else if r > 248 {
            231
        } else {
            ((r as u16 - 8) * 24 / 247) as u8 + 232
        }
    } else {
        let r = (r as u16 * 5 / 255) as u8;
        let g = (g as u16 * 5 / 255) as u8;
        let b = (b as u16 * 5 / 255) as u8;
        16 + 36 * r + 6 * g + b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read};

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_defaults_without_args() {
        let cfg = Config::parse(&[]).unwrap();
        assert_eq!(cfg.spread, 3.0);
        assert_eq!(cfg.freq, 0.1);
        assert_eq!(cfg.seed, 0);
        assert!(!cfg.animate);
        assert_eq!(cfg.duration, 12);
        assert_eq!(cfg.speed, 20.0);
        assert!(!cfg.invert);
        assert!(!cfg.truecolor);
        assert!(!cfg.force);
        assert!(cfg.files.is_empty());
    }

    #[test]
    fn parse_mixed_short_and_long_options() {
        let args = strings(&[
            "-p",
            "5",
            "--freq=0.2",
            "-S7",
            "--duration",
            "6",
            "-s15.5",
            "-aitf",
            "foo",
            "-",
            "bar",
        ]);
        let cfg = Config::parse(&args).unwrap();
        assert_eq!(cfg.spread, 5.0);
        assert!((cfg.freq - 0.2).abs() < f64::EPSILON);
        assert_eq!(cfg.seed, 7);
        assert!(cfg.animate);
        assert_eq!(cfg.duration, 6);
        assert!((cfg.speed - 15.5).abs() < f64::EPSILON);
        assert!(cfg.invert);
        assert!(cfg.truecolor);
        assert!(cfg.force);
        assert_eq!(
            cfg.files,
            vec!["foo".to_string(), "-".to_string(), "bar".to_string()]
        );
    }

    #[test]
    fn parse_requires_values() {
        let err = Config::parse(&strings(&["-p"])).unwrap_err();
        assert!(err.contains("-p"), "unexpected error: {err}");
        assert!(err.contains("requires"));
    }

    #[test]
    fn animate_option_consumes_numeric_duration() {
        let cfg = Config::parse(&strings(&["--animate", "1.4"])).unwrap();
        assert!(cfg.animate);
        assert_eq!(cfg.duration, 1);
        assert!(cfg.files.is_empty());
    }

    #[test]
    fn animate_short_form_consumes_numeric_duration() {
        let cfg = Config::parse(&strings(&["-a", "2"])).unwrap();
        assert!(cfg.animate);
        assert_eq!(cfg.duration, 2);
        assert!(cfg.files.is_empty());
    }

    #[test]
    fn animate_option_leaves_non_numeric_arguments() {
        let cfg = Config::parse(&strings(&["--animate", "foo"])).unwrap();
        assert!(cfg.animate);
        assert_eq!(cfg.files, vec!["foo".to_string()]);
    }

    #[test]
    fn validate_rejects_small_spread() {
        let err = Config::parse(&strings(&["--spread=0.01"])).unwrap_err();
        assert!(err.contains("spread"), "unexpected error: {err}");
    }

    #[test]
    fn duration_conversion_rounds_and_bounds() {
        assert_eq!(float_duration_to_frames(3.2).unwrap(), 3);
        assert_eq!(float_duration_to_frames(0.15).unwrap(), 1);
        assert!(float_duration_to_frames(0.05).is_err());
    }

    #[test]
    fn choose_color_mode_prefers_truecolor_flag() {
        let mut cfg = Config::default();
        cfg.truecolor = true;
        assert!(matches!(
            choose_color_mode_from(&cfg, None),
            ColorMode::TrueColor
        ));
        cfg.truecolor = false;
        assert!(matches!(
            choose_color_mode_from(&cfg, Some("24bit")),
            ColorMode::TrueColor
        ));
        assert!(matches!(
            choose_color_mode_from(&cfg, Some("ansi")),
            ColorMode::Ansi256
        ));
    }

    #[test]
    fn detects_truecolor_env_toggle() {
        assert!(detects_truecolor_from(Some("truecolor")));
        assert!(detects_truecolor_from(Some("24BIT")));
        assert!(!detects_truecolor_from(Some("ansi")));
        assert!(!detects_truecolor_from(None));
    }

    #[test]
    fn streaming_preserves_escape_sequences() {
        let cfg = Config {
            force: true,
            ..Config::default()
        };
        let mut printer = Printer::new(&cfg, true, ColorMode::Ansi256, 0.0);
        let mut output = Vec::new();
        let input = b"\x1b[31mhello\nworld";
        let reader = Chunked::new(&input[..], 2);

        process_stream_streaming(reader, &mut output, &mut printer).unwrap();

        assert!(
            output.windows(5).any(|w| w == b"\x1b[31m"),
            "escape sequence missing in {:?}",
            output
        );
    }

    #[test]
    fn streaming_replaces_invalid_utf8() {
        let cfg = Config {
            force: true,
            ..Config::default()
        };
        let mut printer = Printer::new(&cfg, true, ColorMode::Ansi256, 0.0);
        let mut output = Vec::new();
        let input = [0xFF, 0xFF, b'\n'];
        let reader = Chunked::new(&input, 1);

        process_stream_streaming(reader, &mut output, &mut printer).unwrap();

        assert!(
            output.windows(3).any(|w| w == &[0xEF, 0xBF, 0xBD]),
            "replacement char missing in {:?}",
            output
        );
    }

    #[test]
    fn rgb_to_ansi256_maps_primary_colors() {
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 243);
    }

    struct Chunked<'a> {
        data: &'a [u8],
        pos: usize,
        chunk: usize,
    }

    impl<'a> Chunked<'a> {
        fn new(data: &'a [u8], chunk: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk,
            }
        }
    }

    impl Read for Chunked<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let remaining = self.data.len() - self.pos;
            let take = remaining.min(self.chunk).min(buf.len());
            buf[..take].copy_from_slice(&self.data[self.pos..self.pos + take]);
            self.pos += take;
            Ok(take)
        }
    }
}
