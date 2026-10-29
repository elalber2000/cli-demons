use std::{
    env,
    io::{self, Read, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};
use vt100::{Color as VtColor, Parser};

const MASCOT_WIDTH: u16 = 8;

const TOPS: [&str; 9] = [
    "/\\_/\\", "/\\ /\\", "^   ^", "v   v", " .-. ", " ___ ", " /\\ ", "  ^  ", " ._. ",
];
const SHELLS: [(&str, &str); 4] = [("(", ")"), ("[", "]"), ("{", "}"), ("<", ">")];
const EYES: [(&str, &str); 7] = [
    ("•", "•"),
    ("o", "o"),
    ("O", "O"),
    (".", "."),
    ("0", "0"),
    ("o", "O"),
    ("O", "o"),
];
const MOUTHS: [&str; 8] = [".", "_", "o", "v", "w", "3", "ω", "ᴗ"];
const BODIES: [&str; 9] = [
    " /|\\ ", " / \\ ", " >^< ", " /V\\ ", " |_| ", " [_] ", " /\\ ", " \\_/ ", "  Y  ",
];

const TOP_CODES: &[u8] = b"zvmkgnbrd";
const SHELL_CODES: &[u8] = b"aeou";
const EYE_CODES: &[u8] = b"zvmrklg";
const MOUTH_CODES: &[u8] = b"aeiouyrh";
const BODY_CODES: &[u8] = b"rnksxmltz";
const STATUS_MARKER: &[u8] = b"\x1b]DSTAT:";
const DEMON_MARKER: &[u8] = b"\x1b]DEMON:";
const FOOD: [char; 3] = ['*', '#', '@'];

#[derive(Clone, Copy)]
struct Demon {
    top: usize,
    shell: usize,
    eyes: usize,
    mouth: usize,
    body: usize,
}

struct FeedAnimation {
    food: char,
    started: Instant,
}

enum EyeMood {
    Normal,
    Waiting(Instant),
    Sleep(Instant),
    FastTyping(Instant),
    Success(Instant),
    Failure(Instant),
}

struct App {
    command_line: String,
    ctrl_c_presses: u8,
    should_quit: bool,
    demon: Demon,
    blink_until: Option<Instant>,
    next_blink: Instant,
    random_seed: u64,
    eye_mood: EyeMood,
    last_key_at: Option<Instant>,
    rm_warning: bool,
    feed: Option<FeedAnimation>,
    spawn_started: Option<Instant>,
    ignore_next_status: bool,
}

impl App {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            command_line: String::new(),
            ctrl_c_presses: 0,
            should_quit: false,
            demon: random_demon(),
            blink_until: None,
            next_blink: now + Duration::from_millis(2500),
            random_seed: now.elapsed().as_nanos() as u64 ^ std::process::id() as u64,
            eye_mood: EyeMood::Normal,
            last_key_at: None,
            rm_warning: false,
            feed: None,
            spawn_started: Some(now),
            ignore_next_status: false,
        }
    }

    fn update_animation(&mut self, now: Instant) {
        if let Some(feed) = &self.feed {
            if now.duration_since(feed.started) >= Duration::from_millis(1500) {
                self.feed = None;
            } else {
                return;
            }
        }

        if let Some(started) = self.spawn_started {
            if now.duration_since(started) >= Duration::from_millis(1200) {
                self.spawn_started = None;
            } else {
                return;
            }
        }

        match self.eye_mood {
            EyeMood::Sleep(until)
            | EyeMood::FastTyping(until)
            | EyeMood::Success(until)
            | EyeMood::Failure(until)
                if now >= until =>
            {
                self.eye_mood = EyeMood::Normal;
            }
            _ => {}
        }

        if !matches!(self.eye_mood, EyeMood::Normal) {
            self.blink_until = None;
            return;
        }

        if let Some(until) = self.blink_until {
            if now >= until {
                self.blink_until = None;
                self.next_blink = now + self.next_blink_delay();
            }
        } else if now >= self.next_blink {
            self.blink_until = Some(now + Duration::from_millis(160));
        }
    }

    fn next_blink_delay(&mut self) -> Duration {
        Duration::from_millis(1000 + next_random(&mut self.random_seed, 3000) as u64)
    }

    fn command_finished(&mut self, success: bool) {
        if self.ignore_next_status {
            self.ignore_next_status = false;
            return;
        }
        self.rm_warning = false;
        self.eye_mood = if success {
            EyeMood::Success(Instant::now() + Duration::from_secs(1))
        } else {
            EyeMood::Failure(Instant::now() + Duration::from_secs(1))
        };
        self.blink_until = None;
    }

    fn note_typing(&mut self) {
        let now = Instant::now();
        if self
            .last_key_at
            .is_some_and(|last| now.duration_since(last) <= Duration::from_millis(80))
        {
            self.eye_mood = EyeMood::FastTyping(now + Duration::from_secs(1));
            self.blink_until = None;
        }
        self.last_key_at = Some(now);
    }

    fn start_feed(&mut self) {
        self.feed = Some(FeedAnimation {
            food: FOOD[next_random(&mut self.random_seed, FOOD.len())],
            started: Instant::now(),
        });
        self.eye_mood = EyeMood::Normal;
        self.blink_until = None;
        self.rm_warning = false;
    }

    fn spawn_demon(&mut self, demon: Demon) {
        self.demon = demon;
        self.spawn_started = Some(Instant::now());
        self.eye_mood = EyeMood::Normal;
        self.blink_until = None;
        self.rm_warning = false;
    }

    fn handle_demon_marker(&mut self, marker: &str) {
        self.ignore_next_status = true;
        match marker {
            "help" => {}
            "sleep" => {
                self.eye_mood = EyeMood::Sleep(Instant::now() + Duration::from_secs(3));
                self.blink_until = None;
            }
            "feed" => self.start_feed(),
            "kill" => self.should_quit = true,
            marker => {
                if let Some(name) = marker.strip_prefix("spawn:") {
                    if name.is_empty() {
                        self.spawn_demon(random_demon());
                    } else if let Some(demon) = named_demon(name) {
                        self.spawn_demon(demon);
                    }
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    match env::args().nth(1).as_deref() {
        Some("spawn") => spawn(),
        Some("help") | None => {
            println!("usage: demon spawn");
            Ok(())
        }
        Some(command) => {
            eprintln!("unknown command: {command}\nusage: demon spawn");
            Ok(())
        }
    }
}

fn spawn() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let size = terminal.size()?;
    let pty_size = PtySize {
        rows: size.height,
        cols: size.width.saturating_sub(MASCOT_WIDTH),
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(pty_size).map_err(pty_error)?;
    let shell = env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let mut shell_command = CommandBuilder::new(shell);
    shell_command.cwd(env::current_dir()?);
    if env::var("SHELL")
        .ok()
        .and_then(|shell| shell.rsplit('/').next().map(str::to_owned))
        .as_deref()
        == Some("bash")
    {
        shell_command.env(
            "PROMPT_COMMAND",
            r#"__demon_status=$?; demon() { case "$1" in help) printf 'demon spawn       random demon\ndemon spawn NAME  summon a demon\ndemon feed        feed the demon\ndemon sleep       make the demon sleep\ndemon help        show this help\ndemon kill        exit the TUI\n'; printf '\033]DEMON:help\007';; spawn) case "$2" in ""|[zvmkgnbrd][aeou][zvmrklg][aeiouyrh][rnksxmltz]) printf '\033]DEMON:spawn:%s\007' "$2";; *) printf 'demon: invalid demon name\n';; esac;; feed) printf '\033]DEMON:feed\007';; sleep) printf '\033]DEMON:sleep\007';; kill) printf '\033]DEMON:kill\007';; esac; }; printf '\033]DSTAT:%s\007' "$__demon_status""#,
        );
    }
    let mut child = pair.slave.spawn_command(shell_command).map_err(pty_error)?;
    let mut writer = pair.master.take_writer().map_err(pty_error)?;
    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let (output_sender, output_receiver) = mpsc::channel();

    thread::spawn(move || read_pty(reader, output_sender));

    let mut parser = Parser::new(pty_size.rows, pty_size.cols, 0);
    let mut app = App::new();
    let mut pending_output = Vec::new();

    while !app.should_quit {
        app.update_animation(Instant::now());
        while let Ok(bytes) = output_receiver.try_recv() {
            process_pty_output(&bytes, &mut pending_output, &mut parser, &mut app);
        }

        terminal.draw(|frame| draw(frame, &app, parser.screen()))?;

        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, &mut writer, key)?,
                Event::Resize(width, height) => {
                    let cols = width.saturating_sub(MASCOT_WIDTH);
                    pair.master
                        .resize(PtySize {
                            rows: height,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .map_err(pty_error)?;
                    parser = Parser::new(height, cols, 0);
                }
                _ => {}
            }
        }
    }

    let _ = child.kill();
    Ok(())
}

fn read_pty(mut reader: Box<dyn Read + Send>, sender: mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                if sender.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

fn process_pty_output(bytes: &[u8], pending: &mut Vec<u8>, parser: &mut Parser, app: &mut App) {
    pending.extend_from_slice(bytes);

    loop {
        let status_start = find_bytes(pending, STATUS_MARKER);
        let demon_start = find_bytes(pending, DEMON_MARKER);
        let Some((start, is_demon)) = (match (status_start, demon_start) {
            (Some(status), Some(demon)) if demon < status => Some((demon, true)),
            (Some(status), _) => Some((status, false)),
            (None, Some(demon)) => Some((demon, true)),
            (None, None) => None,
        }) else {
            let keep = partial_marker_len(pending, STATUS_MARKER)
                .max(partial_marker_len(pending, DEMON_MARKER));
            let process_len = pending.len().saturating_sub(keep);
            if process_len > 0 {
                parser.process(&pending[..process_len]);
                pending.drain(..process_len);
            }
            return;
        };

        if start > 0 {
            parser.process(&pending[..start]);
            pending.drain(..start);
        }

        let marker = if is_demon {
            DEMON_MARKER
        } else {
            STATUS_MARKER
        };
        let Some(end) = pending[marker.len()..]
            .iter()
            .position(|byte| *byte == 0x07)
        else {
            return;
        };

        let payload_start = marker.len();
        let payload_end = payload_start + end;
        if is_demon {
            if let Ok(marker) = std::str::from_utf8(&pending[payload_start..payload_end]) {
                app.handle_demon_marker(marker);
            }
        } else {
            let success = &pending[payload_start..payload_end] == b"0";
            app.command_finished(success);
        }
        pending.drain(..=payload_end);
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn partial_marker_len(bytes: &[u8], marker: &[u8]) -> usize {
    (1..marker.len())
        .rev()
        .find(|length| bytes.ends_with(&marker[..*length]))
        .unwrap_or(0)
}

fn random_demon() -> Demon {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    let mut seed = nanos ^ (std::process::id() as u64);

    Demon {
        top: next_random(&mut seed, TOPS.len()),
        shell: next_random(&mut seed, SHELLS.len()),
        eyes: next_random(&mut seed, EYES.len()),
        mouth: next_random(&mut seed, MOUTHS.len()),
        body: next_random(&mut seed, BODIES.len()),
    }
}

fn next_random(seed: &mut u64, length: usize) -> usize {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed as usize) % length
}

fn named_demon(name: &str) -> Option<Demon> {
    let name = name.to_ascii_lowercase();
    let bytes = name.as_bytes();
    if bytes.len() != 5 {
        return None;
    }

    Some(Demon {
        top: TOP_CODES.iter().position(|code| *code == bytes[0])?,
        shell: SHELL_CODES.iter().position(|code| *code == bytes[1])?,
        eyes: EYE_CODES.iter().position(|code| *code == bytes[2])?,
        mouth: MOUTH_CODES.iter().position(|code| *code == bytes[3])?,
        body: BODY_CODES.iter().position(|code| *code == bytes[4])?,
    })
}

fn pty_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn handle_key(app: &mut App, writer: &mut Box<dyn Write + Send>, key: KeyEvent) -> io::Result<()> {
    if key.kind != KeyEventKind::Press {
        return Ok(());
    }

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.ctrl_c_presses += 1;
        app.command_line.clear();
        app.rm_warning = false;
        if app.ctrl_c_presses >= 2 {
            app.should_quit = true;
        } else {
            writer.write_all(&[3])?;
            writer.flush()?;
        }
        return Ok(());
    }

    app.ctrl_c_presses = 0;
    if matches!(key.code, KeyCode::Char(_)) && !key.modifiers.contains(KeyModifiers::CONTROL) {
        app.note_typing();
    }
    let submitted_command = key.code == KeyCode::Enter && !app.command_line.trim().is_empty();

    let bytes = key_bytes(key);
    if key.code == KeyCode::Enter {
        app.command_line.clear();
    } else if key.code == KeyCode::Backspace {
        app.command_line.pop();
    } else if let KeyCode::Char(character) = key.code {
        if !key.modifiers.contains(KeyModifiers::CONTROL) {
            app.command_line.push(character);
        }
    }
    if key.code != KeyCode::Enter {
        app.rm_warning = app.command_line.contains("rm");
    }

    if !bytes.is_empty() {
        writer.write_all(&bytes)?;
        writer.flush()?;
        if submitted_command {
            app.eye_mood = EyeMood::Waiting(Instant::now());
            app.blink_until = None;
        }
    }
    Ok(())
}

fn key_bytes(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(character) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                vec![(character as u8) & 0x1f]
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                vec![0x1b, character as u8]
            } else {
                character.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => Vec::new(),
    }
}

fn draw(frame: &mut Frame, app: &App, screen: &vt100::Screen) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(MASCOT_WIDTH), Constraint::Min(1)])
        .split(frame.area());

    draw_mascot(frame, columns[0], app);
    draw_screen(frame, columns[1], app, screen);
}

fn draw_mascot(frame: &mut Frame, area: Rect, app: &App) {
    let demon = app.demon;
    let group_height = 7;
    let y = area.y + area.height.saturating_sub(group_height) / 2;
    let (eyes, mouth, food_row, food) = if let Some(feed) = &app.feed {
        let elapsed = feed.started.elapsed();
        if elapsed < Duration::from_millis(250) {
            (
                ("^", "^"),
                MOUTHS[demon.mouth].to_owned(),
                Some(0),
                Some(feed.food),
            )
        } else if elapsed < Duration::from_millis(500) {
            (
                ("^", "^"),
                MOUTHS[demon.mouth].to_owned(),
                Some(1),
                Some(feed.food),
            )
        } else if elapsed < Duration::from_millis(750) {
            (
                ("^", "^"),
                MOUTHS[demon.mouth].to_owned(),
                Some(2),
                Some(feed.food),
            )
        } else if elapsed < Duration::from_millis(1000) {
            (
                ("^", "^"),
                MOUTHS[demon.mouth].to_owned(),
                Some(3),
                Some(feed.food),
            )
        } else if elapsed < Duration::from_millis(1150) {
            (("^", "^"), feed.food.to_string(), None, None)
        } else {
            (("^", "^"), "ᴗ".to_owned(), None, None)
        }
    } else {
        let eyes = match app.eye_mood {
            EyeMood::FastTyping(_) => ("@", "@"),
            EyeMood::Sleep(_) => ("-", "-"),
            EyeMood::Waiting(_) if app.rm_warning => ("0", "0"),
            EyeMood::Waiting(since) if since.elapsed() >= Duration::from_secs(3) => ("z", "z"),
            EyeMood::Waiting(since) if since.elapsed() >= Duration::from_secs(1) => ("-", "-"),
            EyeMood::Waiting(_) => EYES[demon.eyes],
            EyeMood::Success(_) => ("^", "^"),
            EyeMood::Failure(_) => (">", "<"),
            EyeMood::Normal if app.rm_warning => ("0", "0"),
            EyeMood::Normal if app.blink_until.is_some() => ("-", "-"),
            EyeMood::Normal => EYES[demon.eyes],
        };
        (eyes, MOUTHS[demon.mouth].to_owned(), None, None)
    };
    let shell = SHELLS[demon.shell];
    let top = TOPS[demon.top];
    let face = format!("{}{}{}{}{}", shell.0, eyes.0, mouth, eyes.1, shell.1);
    let mut lines = [top.to_owned(), face.clone(), BODIES[demon.body].to_owned()];
    if let Some(started) = app.spawn_started {
        let elapsed = started.elapsed();
        lines = if elapsed < Duration::from_millis(200) {
            [String::new(), String::new(), "  ·  ".to_owned()]
        } else if elapsed < Duration::from_millis(400) {
            [String::new(), String::new(), BODIES[demon.body].to_owned()]
        } else if elapsed < Duration::from_millis(600) {
            [
                String::new(),
                TOPS[demon.top].to_owned(),
                BODIES[demon.body].to_owned(),
            ]
        } else if elapsed < Duration::from_millis(900) {
            [top.to_owned(), face.clone(), BODIES[demon.body].to_owned()]
        } else {
            [
                top.to_owned(),
                format!("{}^ᴗ^{}", shell.0, shell.1),
                BODIES[demon.body].to_owned(),
            ]
        };
    }
    frame.render_widget(
        Paragraph::new(lines.join("\n")).alignment(Alignment::Center),
        Rect {
            x: area.x,
            y: y + 2,
            width: area.width,
            height: 3.min(area.height),
        },
    );

    if app.rm_warning && app.spawn_started.is_none() {
        frame.render_widget(
            Paragraph::new("!").alignment(Alignment::Center),
            Rect {
                x: area.x,
                y: y + 1,
                width: area.width,
                height: 1,
            },
        );
    }

    if app.spawn_started.is_none() {
        if let (Some(food_row), Some(food)) = (food_row, food) {
            frame.render_widget(
                Paragraph::new(food.to_string()).alignment(Alignment::Center),
                Rect {
                    x: area.x,
                    y: y + food_row,
                    width: area.width,
                    height: 1,
                },
            );
        }
    }

    let show_name = app.spawn_started.map_or(true, |started| {
        started.elapsed() >= Duration::from_millis(600)
    });
    if area.height >= group_height && show_name {
        let name = demon_name(demon);
        frame.render_widget(
            Paragraph::new(name).alignment(Alignment::Center),
            Rect {
                x: area.x,
                y: y + 6,
                width: area.width,
                height: 1,
            },
        );
    }
}

fn demon_name(demon: Demon) -> String {
    let name = [
        TOP_CODES[demon.top],
        SHELL_CODES[demon.shell],
        EYE_CODES[demon.eyes],
        MOUTH_CODES[demon.mouth],
        BODY_CODES[demon.body],
    ];
    let mut name = String::from_utf8(name.to_vec()).expect("demon codes are ASCII");
    name.get_mut(0..1)
        .expect("demon names have five letters")
        .make_ascii_uppercase();
    name
}

fn draw_screen(frame: &mut Frame, area: Rect, app: &App, screen: &vt100::Screen) {
    let (rows, columns) = screen.size();
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut spans = Vec::new();
        for column in 0..columns {
            if let Some(cell) = screen.cell(row, column) {
                let mut style = Style::default()
                    .fg(vt_color(cell.fgcolor()))
                    .bg(vt_color(cell.bgcolor()));
                let mut modifiers = Modifier::empty();
                if cell.bold() {
                    modifiers |= Modifier::BOLD;
                }
                if cell.italic() {
                    modifiers |= Modifier::ITALIC;
                }
                if cell.underline() {
                    modifiers |= Modifier::UNDERLINED;
                }
                if cell.inverse() {
                    modifiers |= Modifier::REVERSED;
                }
                style = style.add_modifier(modifiers);
                spans.push(Span::styled(cell.contents().to_owned(), style));
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
    frame.set_cursor_position((
        area.x + screen.cursor_position().1,
        area.y + screen.cursor_position().0,
    ));

    let _ = app;
}

fn vt_color(color: VtColor) -> Color {
    match color {
        VtColor::Default => Color::Reset,
        VtColor::Idx(index) => Color::Indexed(index),
        VtColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}
