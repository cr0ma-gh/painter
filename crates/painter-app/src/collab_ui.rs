//! Shared drawing sessions (Window → Panels → Session), inspired by
//! Drawpile, in the app itself on every platform: one person hosts (Painter
//! listens on a TCP port), the others join with the host's address. Every
//! change goes to everyone through the host; see `painter_core::collab` for
//! what travels. Each person sees the others' pointers with their names, and
//! there's a chat.
//!
//! Over the internet the host's port has to be reachable (forwarded on the
//! router, or a VPN such as Tailscale or ZeroTier); on the same network the
//! local address is enough.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, RichText, Stroke};
use painter_core::collab::{self, Message, Peer, PlaneId, MAX_FRAME, VERSION};
use painter_core::{Document, LayerId, TileCoord, TileStore};
use painter_render::Plane;

use crate::app::PainterApp;
use crate::theme;

pub(crate) const DEFAULT_PORT: u16 = 27750;
/// How often local changes are sent at most.
const SYNC_EVERY: Duration = Duration::from_millis(120);
/// Tiles per message, so big changes go in pieces.
const TILES_PER_MESSAGE: usize = 96;

type Frame = Arc<Vec<u8>>;

enum NetEvent {
    Connected { conn: u32, tx: Sender<Frame> },
    Frame { conn: u32, message: Message },
    Closed { conn: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Host,
    Client,
}

struct Conn {
    tx: Sender<Frame>,
    /// Set once it said hello and was let in.
    peer: Option<u32>,
}

pub(crate) struct Session {
    role: Role,
    me: u32,
    peers: Vec<Peer>,
    events: Receiver<NetEvent>,
    conns: HashMap<u32, Conn>,
    /// The last state everyone shares; local changes are what differs from it.
    baseline: Option<Document>,
    /// The document tab the session is about.
    doc_id: u64,
    last_sync: Instant,
    password: String,
    next_peer: u32,
    cursors: HashMap<u32, (Pos2, Instant)>,
    last_cursor: Instant,
    pub chat: Vec<(String, String)>,
    /// Where others reach the host.
    pub address: String,
    /// Tells the listening thread to close the port.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The Session panel's fields before a session starts.
pub(crate) struct SessionForm {
    name: String,
    host_port: String,
    host_password: String,
    join_address: String,
    join_password: String,
    chat_input: String,
}

impl Default for SessionForm {
    fn default() -> Self {
        let name = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "Painter".into());
        Self {
            name,
            host_port: DEFAULT_PORT.to_string(),
            host_password: String::new(),
            join_address: String::new(),
            join_password: String::new(),
            chat_input: String::new(),
        }
    }
}

/// Reads frames from `stream` into events until it closes.
fn reader(conn: u32, mut stream: TcpStream, events: Sender<NetEvent>) {
    loop {
        let mut len = [0u8; 4];
        if stream.read_exact(&mut len).is_err() {
            break;
        }
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME {
            break;
        }
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            break;
        }
        // Malformed frames are dropped; the connection stays.
        if let Some(message) = Message::decode(&body) {
            if events.send(NetEvent::Frame { conn, message }).is_err() {
                return;
            }
        }
    }
    let _ = events.send(NetEvent::Closed { conn });
}

/// Starts the reading and writing threads of a connection; returns its sender.
fn start_conn(conn: u32, stream: TcpStream, events: Sender<NetEvent>) -> std::io::Result<Sender<Frame>> {
    stream.set_nodelay(true).ok();
    let read_half = stream.try_clone()?;
    let (tx, rx) = channel::<Frame>();
    std::thread::Builder::new().name(format!("session-read-{conn}")).spawn(move || reader(conn, read_half, events))?;
    let mut write_half = stream;
    std::thread::Builder::new().name(format!("session-write-{conn}")).spawn(move || {
        for frame in rx {
            if write_half.write_all(&frame).is_err() {
                break;
            }
        }
        let _ = write_half.shutdown(std::net::Shutdown::Both);
    })?;
    Ok(tx)
}

/// This computer's address on the local network, as others would use it.
fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // No packet is sent: connecting a UDP socket only picks the route.
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// A colour per person, for their pointer and name.
fn peer_color(id: u32) -> Color32 {
    let hue = (id as f32 * 0.61803) % 1.0;
    egui::ecolor::Hsva::new(hue, 0.65, 0.95, 1.0).into()
}

fn to_plane(p: PlaneId) -> Plane {
    if p.mask {
        Plane::Mask(LayerId(p.layer))
    } else {
        Plane::Color(LayerId(p.layer))
    }
}

fn from_plane(p: Plane) -> Option<PlaneId> {
    match p {
        Plane::Color(id) => Some(PlaneId { layer: id.0, mask: false }),
        Plane::Mask(id) => Some(PlaneId { layer: id.0, mask: true }),
        _ => None,
    }
}

impl Session {
    fn send_to(&self, conn: u32, message: &Message) {
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.tx.send(Arc::new(message.encode()));
        }
    }

    /// To everyone in the session (but `except`, a connection).
    fn broadcast(&self, message: &Message, except: Option<u32>) {
        let frame: Frame = Arc::new(message.encode());
        for (id, c) in &self.conns {
            if Some(*id) != except && (c.peer.is_some() || self.role == Role::Client) {
                let _ = c.tx.send(frame.clone());
            }
        }
    }

    fn name_of(&self, id: u32) -> String {
        self.peers.iter().find(|p| p.id == id).map_or_else(|| format!("#{id}"), |p| p.name.clone())
    }
}

impl PainterApp {
    /// Starts hosting on `port`.
    fn host_session(&mut self, port: u16, password: String) {
        self.host_session_on("0.0.0.0", port, password);
    }

    /// Hosting on one interface (`127.0.0.1`: this computer only).
    fn host_session_on(&mut self, interface: &str, port: u16, password: String) {
        let listener = match TcpListener::bind((interface, port)) {
            Ok(l) => l,
            Err(e) => {
                self.status = format!("Couldn't listen on port {port}: {e}");
                return;
            }
        };
        let (events_tx, events) = channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        // Polled, so leaving the session closes the port.
        let _ = listener.set_nonblocking(true);
        std::thread::Builder::new()
            .name("session-listen".into())
            .spawn(move || {
                let mut next = 1u32;
                while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let conn = next;
                            next += 1;
                            if let Ok(tx) = start_conn(conn, stream, events_tx.clone()) {
                                if events_tx.send(NetEvent::Connected { conn, tx }).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(150)),
                    }
                }
            })
            .ok();
        let name = self.session_form.name.trim().to_string();
        let ip = if interface == "0.0.0.0" {
            local_ip().unwrap_or_else(|| "this computer's address".into())
        } else {
            interface.to_string()
        };
        let address = format!("{ip}:{port}");
        self.status = format!("Hosting a session: others join at {address}");
        self.session = Some(Session {
            role: Role::Host,
            me: 0,
            peers: vec![Peer { id: 0, name: if name.is_empty() { "Host".into() } else { name } }],
            events,
            conns: HashMap::new(),
            baseline: Some(self.doc.clone()),
            doc_id: self.docs.active_id(),
            last_sync: Instant::now(),
            password,
            next_peer: 1,
            cursors: HashMap::new(),
            last_cursor: Instant::now(),
            chat: Vec::new(),
            address,
            stop,
        });
    }

    /// Joins the session at `address` (host:port).
    fn join_session(&mut self, address: &str, password: String) {
        let address = if address.contains(':') { address.to_string() } else { format!("{address}:{DEFAULT_PORT}") };
        let stream = match std::net::ToSocketAddrs::to_socket_addrs(&address.as_str())
            .ok()
            .and_then(|mut a| a.next())
            .and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).ok())
        {
            Some(s) => s,
            None => {
                self.status = format!("Couldn't reach {address}");
                return;
            }
        };
        let (events_tx, events) = channel();
        let tx = match start_conn(0, stream, events_tx) {
            Ok(tx) => tx,
            Err(e) => {
                self.status = format!("Couldn't join: {e}");
                return;
            }
        };
        let name = self.session_form.name.trim().to_string();
        let _ = tx.send(Arc::new(
            Message::Hello { name: if name.is_empty() { "Guest".into() } else { name }, password, version: VERSION }.encode(),
        ));
        let mut conns = HashMap::new();
        conns.insert(0, Conn { tx, peer: Some(0) });
        self.status = format!("Joining {address}…");
        self.session = Some(Session {
            role: Role::Client,
            me: u32::MAX,
            peers: Vec::new(),
            events,
            conns,
            // Set when the host's document arrives.
            baseline: None,
            doc_id: 0,
            last_sync: Instant::now(),
            password: String::new(),
            next_peer: 0,
            cursors: HashMap::new(),
            last_cursor: Instant::now(),
            chat: Vec::new(),
            address,
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
    }

    pub(crate) fn leave_session(&mut self) {
        // Dropping it closes the connections (their writers end) and the port.
        if self.session.take().is_some() {
            self.status = "Left the session".into();
        }
    }

    /// Whether the shared document is the one on screen (else the session waits).
    fn session_here(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.baseline.is_some() && s.doc_id == self.docs.active_id())
    }

    /// Network events, then local changes out. Every frame.
    pub(crate) fn session_tick(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else { return };
        let mut events = Vec::new();
        while let Ok(e) = session.events.try_recv() {
            events.push(e);
        }
        for e in events {
            self.session_event(e);
            if self.session.is_none() {
                return;
            }
        }
        if self.session_here() {
            self.session_sync(false);
            self.session_cursor(ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn session_event(&mut self, event: NetEvent) {
        let Some(s) = self.session.as_mut() else { return };
        match event {
            NetEvent::Connected { conn, tx } => {
                s.conns.insert(conn, Conn { tx, peer: None });
            }
            NetEvent::Closed { conn } => {
                if s.role == Role::Client {
                    self.session = None;
                    self.status = "The session ended (the host left or the connection dropped)".into();
                    return;
                }
                if let Some(peer) = s.conns.remove(&conn).and_then(|c| c.peer) {
                    let name = s.name_of(peer);
                    s.peers.retain(|p| p.id != peer);
                    s.cursors.remove(&peer);
                    s.chat.push((String::new(), format!("{name} left")));
                    let peers = s.peers.clone();
                    s.broadcast(&Message::Peers { peers }, None);
                }
            }
            NetEvent::Frame { conn, message } => self.session_message(conn, message),
        }
    }

    fn session_message(&mut self, conn: u32, message: Message) {
        let Some(s) = self.session.as_mut() else { return };
        // The host vouches for who sent what.
        let message = if s.role == Role::Host {
            match (s.conns.get(&conn).and_then(|c| c.peer), message) {
                (None, Message::Hello { name, password, version }) => return self.session_hello(conn, name, password, version),
                (None, _) => return,
                // Only changes, pointers, chat and resync requests come from the others.
                (
                    Some(peer),
                    m @ (Message::Structure { .. }
                    | Message::Tiles { .. }
                    | Message::Stroke { .. }
                    | Message::Cursor { .. }
                    | Message::Chat { .. }
                    | Message::ResyncRequest { .. }),
                ) => with_sender(m, peer),
                (Some(_), _) => return,
            }
        } else {
            message
        };
        match &message {
            Message::Welcome { peer, peers, document } => {
                match painter_core::project::from_bytes(document) {
                    Ok(mut doc) => {
                        doc.ids_from((*peer as u64) << 40);
                        let (peer, peers) = (*peer, peers.clone());
                        self.set_document(doc, None);
                        let Some(s) = self.session.as_mut() else { return };
                        s.me = peer;
                        s.peers = peers;
                        s.baseline = Some(self.doc.clone());
                        s.doc_id = self.docs.active_id();
                        self.status = "Joined the session".into();
                    }
                    Err(e) => {
                        self.session = None;
                        self.status = format!("The host's document couldn't be read: {e}");
                    }
                }
                return;
            }
            Message::Snapshot { document } => {
                if let Ok(doc) = painter_core::project::from_bytes(document) {
                    let me = s.me;
                    if s.doc_id == self.docs.active_id() {
                        let mut doc = doc;
                        doc.ids_from((me as u64) << 40);
                        self.doc = doc;
                        self.history.clear();
                        self.resync_gpu();
                        if let Some(s) = self.session.as_mut() {
                            s.baseline = Some(self.doc.clone());
                        }
                        self.status = "Back in step with the host".into();
                    }
                }
                return;
            }
            Message::Refused { reason } => {
                self.status = format!("The host said no: {reason}");
                self.session = None;
                return;
            }
            Message::Peers { peers } => {
                s.peers = peers.clone();
                return;
            }
            Message::Cursor { from, x, y } => {
                s.cursors.insert(*from, (Pos2::new(*x, *y), Instant::now()));
            }
            Message::Chat { from, text } => {
                let name = s.name_of(*from);
                s.chat.push((name, text.clone()));
            }
            Message::ResyncRequest { .. } if s.role == Role::Host => {
                if let Ok(bytes) = painter_core::project::to_bytes(&self.doc) {
                    s.send_to(conn, &Message::Snapshot { document: bytes });
                }
                return;
            }
            _ => {}
        }
        // Document changes: here (when this is the shared document), then on to the others.
        if self.session_here() {
            self.apply_remote(&message);
        }
        if let Some(s) = self.session.as_ref() {
            if s.role == Role::Host {
                s.broadcast(&message, Some(conn));
            }
        }
    }

    fn session_hello(&mut self, conn: u32, name: String, password: String, version: u32) {
        let Some(s) = self.session.as_mut() else { return };
        let refuse = if version != VERSION {
            Some(format!("this Painter speaks session version {VERSION}, yours {version}: update both to the same version"))
        } else if !s.password.is_empty() && password != s.password {
            Some("wrong password".into())
        } else {
            None
        };
        if let Some(reason) = refuse {
            s.send_to(conn, &Message::Refused { reason });
            return;
        }
        // Everyone in step first, so the newcomer's copy matches.
        self.session_sync(true);
        let document = match painter_core::project::to_bytes(&self.doc) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("Couldn't send the document: {e}");
                return;
            }
        };
        let Some(s) = self.session.as_mut() else { return };
        let peer = s.next_peer;
        s.next_peer += 1;
        let name: String = name.chars().filter(|c| !c.is_control()).take(40).collect();
        s.peers.push(Peer { id: peer, name: name.clone() });
        if let Some(c) = s.conns.get_mut(&conn) {
            c.peer = Some(peer);
        }
        s.send_to(conn, &Message::Welcome { peer, peers: s.peers.clone(), document });
        s.chat.push((String::new(), format!("{name} joined")));
        let peers = s.peers.clone();
        s.broadcast(&Message::Peers { peers }, Some(conn));
    }

    /// A change from someone else: on the document, the shared state, the
    /// undo states and the stroke being drawn, then on screen.
    fn apply_remote(&mut self, message: &Message) {
        let canvas = self.doc.canvas();
        match message {
            Message::Structure { json, .. } => {
                if painter_core::project::apply_structure(&mut self.doc, json).is_err() {
                    return;
                }
                if let Some(base) = self.session.as_mut().and_then(|s| s.baseline.as_mut()) {
                    let _ = painter_core::project::apply_structure(base, json);
                }
                self.history.edit_snapshots(|layers| {
                    let _ = painter_core::project::apply_structure_to(layers, json);
                });
                if self.doc.canvas() != canvas {
                    self.fit_pending = true;
                }
                self.resync_gpu();
            }
            Message::Tiles { plane, tiles, .. } => {
                collab::apply_tiles(&mut self.doc.layers, *plane, tiles);
                if let Some(base) = self.session.as_mut().and_then(|s| s.baseline.as_mut()) {
                    collab::apply_tiles(&mut base.layers, *plane, tiles);
                }
                self.history.edit_snapshots(|layers| {
                    collab::apply_tiles(layers, *plane, tiles);
                });
                if let Some(stroke) = self.stroke.as_mut().filter(|st| from_plane(st.plane) == Some(*plane)) {
                    let mut tmp = vec![painter_core::Layer::raster(LayerId(plane.layer), "")];
                    if let painter_core::LayerKind::Raster(t) = &mut tmp[0].kind {
                        *t = std::mem::take(&mut stroke.before);
                    }
                    collab::apply_tiles(&mut tmp, PlaneId { layer: plane.layer, mask: false }, tiles);
                    if let painter_core::LayerKind::Raster(t) = &mut tmp[0].kind {
                        stroke.before = std::mem::take(t);
                    }
                }
                self.upload_tiles(to_plane(*plane), tiles.iter().map(|(c, _)| *c).collect::<Vec<_>>());
            }
            Message::Stroke { plane, opacity, erase, blend, alpha_locked, tiles, .. } => {
                let apply = |layers: &mut [painter_core::Layer]| {
                    collab::apply_stroke(layers, *plane, *opacity, *erase, *blend, *alpha_locked, tiles)
                };
                apply(&mut self.doc.layers);
                if let Some(base) = self.session.as_mut().and_then(|s| s.baseline.as_mut()) {
                    apply(&mut base.layers);
                }
                self.history.edit_snapshots(|layers| {
                    apply(layers);
                });
                if let Some(stroke) = self.stroke.as_mut().filter(|st| from_plane(st.plane) == Some(*plane)) {
                    let mut tmp = vec![painter_core::Layer::raster(LayerId(plane.layer), "")];
                    if let painter_core::LayerKind::Raster(t) = &mut tmp[0].kind {
                        *t = std::mem::take(&mut stroke.before);
                    }
                    collab::apply_stroke(
                        &mut tmp,
                        PlaneId { layer: plane.layer, mask: false },
                        *opacity,
                        *erase,
                        *blend,
                        *alpha_locked,
                        tiles,
                    );
                    if let painter_core::LayerKind::Raster(t) = &mut tmp[0].kind {
                        stroke.before = std::mem::take(t);
                    }
                }
                self.upload_tiles(to_plane(*plane), tiles.iter().map(|(c, _)| *c).collect::<Vec<_>>());
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Sends what changed here since the shared state (`force`: now, even mid-stroke throttle).
    fn session_sync(&mut self, force: bool) {
        if self.stroke.is_some() || self.transform.is_some() || !self.session_here() {
            return;
        }
        let Some(s) = self.session.as_mut() else { return };
        if !force && s.last_sync.elapsed() < SYNC_EVERY {
            return;
        }
        s.last_sync = Instant::now();
        let Some(base) = s.baseline.as_ref() else { return };
        let (structure, planes) = collab::diff(base, &self.doc);
        if !structure && planes.is_empty() {
            return;
        }
        let me = s.me;
        if structure {
            s.broadcast(&Message::Structure { from: me, json: painter_core::project::structure(&self.doc) }, None);
        }
        for (plane, coords) in planes {
            for chunk in coords.chunks(TILES_PER_MESSAGE) {
                if let Some(m) = collab::tiles_message(me, &self.doc, plane, chunk) {
                    s.broadcast(&m, None);
                }
            }
        }
        s.baseline = Some(self.doc.clone());
    }

    /// A finished stroke or fill: sent as a stroke to merge, and merged into
    /// the shared state too (so it isn't sent again as tiles).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn session_stroke(
        &mut self,
        plane: Plane,
        buffer: &painter_core::raster::StrokeBuffer,
        dirty: &std::collections::BTreeSet<TileCoord>,
        opacity: f32,
        erase: bool,
        blend: painter_core::BrushBlend,
        alpha_locked: bool,
        selection: Option<&TileStore>,
    ) {
        if !self.session_here() {
            return;
        }
        let Some(plane) = from_plane(plane) else { return };
        let Some(s) = self.session.as_mut() else { return };
        let tiles: Vec<(TileCoord, Vec<u8>)> = dirty
            .iter()
            .filter_map(|&c| {
                let pixels = buffer.get(c)?;
                let keep = match selection {
                    Some(sel) => Some(&**sel.get(c)?),
                    None => None,
                };
                Some((c, collab::stroke_tile_bytes(pixels, keep)))
            })
            .collect();
        if tiles.is_empty() {
            return;
        }
        if let Some(base) = s.baseline.as_mut() {
            collab::apply_stroke(&mut base.layers, plane, opacity, erase, blend, alpha_locked, &tiles);
        }
        let me = s.me;
        for chunk in tiles.chunks(TILES_PER_MESSAGE) {
            s.broadcast(&Message::Stroke { from: me, plane, opacity, erase, blend, alpha_locked, tiles: chunk.to_vec() }, None);
        }
    }

    /// Where this pointer is, a few times a second.
    fn session_cursor(&mut self, ctx: &egui::Context) {
        let Some(pos) = ctx.pointer_hover_pos().filter(|p| self.canvas_rect.contains(*p)) else { return };
        let doc = self.view.screen_to_doc(self.canvas_rect, self.doc_size(), pos);
        let Some(s) = self.session.as_mut() else { return };
        if s.last_cursor.elapsed() < Duration::from_millis(60) {
            return;
        }
        s.last_cursor = Instant::now();
        let me = s.me;
        s.broadcast(&Message::Cursor { from: me, x: doc.x, y: doc.y }, None);
    }

    /// The others' pointers, with their names.
    pub(crate) fn draw_session_cursors(&self, painter: &Painter, rect: Rect) {
        let Some(s) = &self.session else { return };
        if !self.session_here() {
            return;
        }
        let doc = self.doc_size();
        for (id, (at, when)) in &s.cursors {
            if when.elapsed() > Duration::from_secs(8) || *id == s.me {
                continue;
            }
            let p = self.view.doc_to_screen(rect, doc, *at);
            let color = peer_color(*id);
            painter.circle_stroke(p, 6.0, Stroke::new(2.0_f32, color));
            painter.circle_filled(p, 2.0, color);
            let name = s.name_of(*id);
            let galley = painter.layout_no_wrap(name, FontId::proportional(11.0), Color32::WHITE);
            let label = Rect::from_min_size(p + egui::vec2(9.0, 6.0), galley.size() + egui::vec2(8.0, 2.0));
            painter.rect_filled(label, 3.0, color.gamma_multiply(0.85));
            painter.galley(label.min + egui::vec2(4.0, 1.0), galley, Color32::WHITE);
        }
    }

    /// The Session panel: host or join; then who's there, the chat, leave.
    pub(crate) fn session_panel(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else {
            let f = &mut self.session_form;
            ui.horizontal(|ui| {
                ui.label("Your name");
                ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(160.0));
            });
            ui.separator();
            ui.label(RichText::new("Host this document").strong());
            ui.horizontal(|ui| {
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut f.host_port).desired_width(60.0));
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut f.host_password).password(true).desired_width(100.0).hint_text("none"));
            });
            let mut host = None;
            if ui.button("Start Hosting").on_hover_text("Others join with this computer's address and the port").clicked() {
                host = Some((f.host_port.trim().parse::<u16>().unwrap_or(DEFAULT_PORT), f.host_password.clone()));
            }
            ui.separator();
            ui.label(RichText::new("Join a session").strong());
            ui.horizontal(|ui| {
                ui.label("Address");
                ui.add(egui::TextEdit::singleline(&mut f.join_address).desired_width(160.0).hint_text("192.168.1.20:27750"));
            });
            ui.horizontal(|ui| {
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut f.join_password).password(true).desired_width(100.0).hint_text("if any"));
            });
            let mut join = None;
            if ui
                .add_enabled(!f.join_address.trim().is_empty(), egui::Button::new("Join"))
                .on_hover_text("The host's document opens in a new tab")
                .clicked()
            {
                join = Some((f.join_address.trim().to_string(), f.join_password.clone()));
            }
            ui.add_space(4.0);
            ui.small("On the same network the host's local address is enough; over the internet the host's port has to be reachable (forwarded on the router, or a VPN such as Tailscale).");
            if let Some((port, pw)) = host {
                self.host_session(port, pw);
            }
            if let Some((addr, pw)) = join {
                self.join_session(&addr, pw);
            }
            return;
        };
        let here = self.session_here();
        let role = if s.role == Role::Host { format!("Hosting at {}", s.address) } else { format!("Joined {}", s.address) };
        ui.label(RichText::new(role).strong());
        if s.role == Role::Client && s.baseline.is_none() {
            ui.label("Waiting for the host's document…");
        } else if !here {
            ui.colored_label(theme::accent(), "Paused: switch back to the shared document's tab");
        }
        ui.horizontal_wrapped(|ui| {
            for p in &s.peers {
                let me = if p.id == s.me { " (you)" } else { "" };
                ui.label(RichText::new(format!("● {}{me}", p.name)).color(peer_color(p.id)));
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
            for (who, text) in &s.chat {
                if who.is_empty() {
                    ui.label(RichText::new(text).small().color(theme::text_dim()));
                } else {
                    ui.label(RichText::new(format!("{who}: {text}")));
                }
            }
        });
        let mut send = None;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.session_form.chat_input)
                    .desired_width(ui.available_width() - 50.0)
                    .hint_text("Say something"),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Send").clicked() || enter) && !self.session_form.chat_input.trim().is_empty() {
                send = Some(std::mem::take(&mut self.session_form.chat_input));
                r.request_focus();
            }
        });
        let mut leave = false;
        let mut resync = false;
        ui.horizontal(|ui| {
            leave = ui.button("Leave").clicked();
            if s.role == Role::Client {
                resync = ui.button("Resync").on_hover_text("Get the host's document again, if things look out of step").clicked();
            }
        });
        ui.small("Undo takes back your own changes; others' strokes stay.");
        if let Some(text) = send {
            if let Some(s) = self.session.as_mut() {
                let me = s.me;
                let msg = Message::Chat { from: me, text: text.trim().to_string() };
                s.chat.push((s.name_of(me), text.trim().to_string()));
                s.broadcast(&msg, None);
            }
        }
        if resync {
            if let Some(s) = &self.session {
                s.broadcast(&Message::ResyncRequest { from: s.me }, None);
            }
        }
        if leave {
            self.leave_session();
        }
    }
}

/// The message as sent by `peer` (the host fills in who sent it).
fn with_sender(m: Message, peer: u32) -> Message {
    match m {
        Message::Structure { json, .. } => Message::Structure { from: peer, json },
        Message::Tiles { plane, tiles, .. } => Message::Tiles { from: peer, plane, tiles },
        Message::Stroke { plane, opacity, erase, blend, alpha_locked, tiles, .. } => {
            Message::Stroke { from: peer, plane, opacity, erase, blend, alpha_locked, tiles }
        }
        Message::Cursor { x, y, .. } => Message::Cursor { from: peer, x, y },
        Message::Chat { text, .. } => Message::Chat { from: peer, text },
        Message::ResyncRequest { .. } => Message::ResyncRequest { from: peer },
        other => other,
    }
}

impl PainterApp {
    /// For screenshot runs (`dev.host:PORT`).
    pub(crate) fn dev_host(&mut self, port: u16) {
        self.session_form.name = "Host".into();
        // This computer only: no firewall question during a test.
        self.host_session_on("127.0.0.1", port, String::new());
    }

    /// For screenshot runs (`dev.join:ADDRESS`).
    pub(crate) fn dev_join(&mut self, address: &str) {
        self.session_form.name = "Guest".into();
        self.join_session(address, String::new());
    }
}

impl PainterApp {
    /// For screenshot runs (`dev.chat:TEXT`): says TEXT in the session.
    pub(crate) fn dev_chat(&mut self, text: &str) {
        if let Some(s) = self.session.as_mut() {
            let me = s.me;
            s.chat.push((s.name_of(me), text.to_string()));
            s.broadcast(&Message::Chat { from: me, text: text.to_string() }, None);
        }
    }
}
