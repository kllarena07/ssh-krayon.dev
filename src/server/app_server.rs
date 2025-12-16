use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;

use crossterm::event::KeyCode;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::server::Handle;
use russh::{Channel, ChannelId, Pty};
use russh::{MethodKind, MethodSet, server::*};
use tokio::sync::Mutex;
use tokio::sync::mpsc::unbounded_channel;

use crate::app::App;
use crate::server::TerminalHandle;

type SshTerminal = Terminal<CrosstermBackend<TerminalHandle>>;

#[derive(Clone)]
pub struct AppServer {
    clients: Arc<Mutex<HashMap<usize, (SshTerminal, App, std::time::Instant, Handle, ChannelId)>>>,
    id: usize,
}

impl AppServer {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            id: 0,
        }
    }

    fn load_host_keys() -> Result<russh::keys::PrivateKey, anyhow::Error> {
        let secrets_location =
            env::var("SECRETS_LOCATION").expect("SECRETS_LOCATION was not defined.");
        let key_path = Path::new(&secrets_location);

        if !key_path.exists() {
            return Err(anyhow::anyhow!(
                "Host key not found at {}. Please generate host keys first.",
                key_path.display()
            ));
        }

        let key = russh::keys::PrivateKey::read_openssh_file(key_path)
            .map_err(|e| anyhow::anyhow!("Failed to read host key: {}", e))?;

        Ok(key)
    }

    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
        let clients = self.clients.clone();
        tokio::spawn(async move {
            let mut tick: u64 = 0;
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000 / 30)).await;

                for (_, (terminal, app, _, _, _)) in clients.lock().await.iter_mut() {
                    app.handle_tick(tick);

                    let _ = terminal.draw(|f| {
                        app.draw(f);
                    });
                }
                tick = tick.wrapping_add(1);
            }
        });

        let clients_timeout = self.clients.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let mut to_remove = Vec::new();
                {
                    let clients_lock = clients_timeout.lock().await;
                    for (&id, (_, _, last_activity, handle, channel_id)) in clients_lock.iter() {
                        if last_activity.elapsed() > std::time::Duration::from_secs(300) {
                            to_remove.push((id, handle.clone(), *channel_id));
                        }
                    }
                }
                for (id, handle, channel_id) in to_remove {
                    let reset_sequence = b"\x1b[0m\x1b[2J\x1b[H\x1b[r\x1b[?25h";
                    let _ = handle
                        .data(channel_id, reset_sequence.as_ref().into())
                        .await;
                    let _ = handle.close(channel_id).await;
                    clients_timeout.lock().await.remove(&id);
                }
            }
        });

        let mut methods = MethodSet::empty();
        methods.push(MethodKind::None);

        println!("Starting SSH server on port 22...");

        let host_key = Self::load_host_keys()
            .map_err(|e| anyhow::anyhow!("Failed to load host keys: {}", e))?;

        let config = Config {
            inactivity_timeout: None,
            auth_rejection_time: std::time::Duration::from_secs(3),
            auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
            methods,
            keys: vec![host_key],
            nodelay: true,
            ..Default::default()
        };

        self.run_on_address(Arc::new(config), ("0.0.0.0", 22))
            .await?;
        Ok(())
    }

    fn map_key_event(data: &[u8]) -> Option<KeyCode> {
        match data {
            b"q" => Some(KeyCode::Char('q')),
            b"Q" => Some(KeyCode::Char('Q')),
            b"\x1b[A" | b"\x1bOA" => Some(KeyCode::Up),
            b"\x1b[B" | b"\x1bOB" => Some(KeyCode::Down),
            b"\x1b[C" | b"\x1bOC" => Some(KeyCode::Right),
            b"\x1b[D" | b"\x1bOD" => Some(KeyCode::Left),
            b"\x1b[5~" => Some(KeyCode::PageUp),
            b"\x1b[6~" => Some(KeyCode::PageDown),
            b"\x1b[H" | b"\x1bOH" => Some(KeyCode::Home),
            b"\x1b[F" | b"\x1bOF" => Some(KeyCode::End),
            b"\t" => Some(KeyCode::Tab),
            b"\x7f" => Some(KeyCode::Backspace),
            b"\x1b[3~" => Some(KeyCode::Delete),
            b"\r" | b"\n" => Some(KeyCode::Enter),
            b" " => Some(KeyCode::Char(' ')),
            [c] if c.is_ascii() && c.is_ascii_graphic() => Some(KeyCode::Char(*c as char)),
            _ => None,
        }
    }
}

impl Server for AppServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        let s = self.clone();
        self.id += 1;
        s
    }
}

impl Handler for AppServer {
    type Error = anyhow::Error;

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let (sender, mut receiver) = unbounded_channel::<Vec<u8>>();
        let channel_id = channel.id();
        let handle = session.handle();
        let handle_clone = handle.clone();

        tokio::spawn(async move {
            while let Some(data) = receiver.recv().await {
                let result = handle_clone.data(channel_id, data.into()).await;
                if result.is_err() {
                    eprintln!("Failed to send data: {result:?}");
                    break;
                }
            }
        });

        let terminal_handle = TerminalHandle::new_with_sender(sender);
        let backend = CrosstermBackend::new(terminal_handle);

        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::default()),
        };

        let terminal = Terminal::with_options(backend, options)?;
        let app = App::new();

        let mut clients = self.clients.lock().await;
        clients.insert(
            self.id,
            (terminal, app, std::time::Instant::now(), handle, channel_id),
        );

        Ok(true)
    }

    async fn auth_none(&mut self, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(key_code) = Self::map_key_event(data) {
            let mut clients = self.clients.lock().await;
            if let Some((_, app, last_activity, _, _)) = clients.get_mut(&self.id) {
                *last_activity = std::time::Instant::now();
                let handle_result = app.handle_key_event(key_code);
                if handle_result.is_err() {
                    // Send terminal reset sequence directly through SSH session
                    let reset_sequence = b"\x1b[0m\x1b[2J\x1b[H\x1b[r\x1b[?25h";
                    let _ = session.data(channel, reset_sequence.as_ref().into());

                    clients.remove(&self.id);
                    session.close(channel)?;
                }
            }
        }

        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _: ChannelId,
        col_width: u32,
        row_height: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: col_width as u16,
            height: row_height as u16,
        };

        let mut clients = self.clients.lock().await;
        if let Some((terminal, _, _, _, _)) = clients.get_mut(&self.id) {
            let _ = terminal.resize(rect);
        }

        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        col_width: u32,
        row_height: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: col_width as u16,
            height: row_height as u16,
        };

        let mut clients = self.clients.lock().await;
        if let Some((terminal, _, _, _, _)) = clients.get_mut(&self.id) {
            let _ = terminal.resize(rect);
        }

        session.channel_success(channel)?;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let mut clients = self.clients.lock().await;

        // Send terminal reset sequence directly through SSH session
        let reset_sequence = b"\x1b[0m\x1b[2J\x1b[H\x1b[r\x1b[?25h";
        let _ = session.data(channel, reset_sequence.as_ref().into());

        clients.remove(&self.id);
        session.close(channel)?;
        Ok(())
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let id = self.id;
        let clients = self.clients.clone();
        // Note: Can't send reset sequence here since we don't have session access
        tokio::spawn(async move {
            let mut clients = clients.lock().await;
            clients.remove(&id);
        });
    }
}
