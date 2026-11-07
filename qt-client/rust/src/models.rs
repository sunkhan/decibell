//! List models over the spike fixture: messages, the member roster and the channel list. Rows are converted
//! to QString once at load, so `data()` hands out implicitly shared copies (no per-call UTF-8 → UTF-16 work
//! while scrolling). After the core split the same models are fed by decibell-core events instead.

use crate::fixture::{self, Fixture};
use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(QString, server_name, cxx_name = "serverName")]
        type ChatModel = super::ChatModelRust;

        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        type RosterModel = super::RosterModelRust;

        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        type ChannelModel = super::ChannelModelRust;
    }

    // ── shared QAbstractItemModel plumbing ──────────────────────────────────────────────────────────
    extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut ChatModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut ChatModel>);
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut RosterModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut RosterModel>);
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut ChannelModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut ChannelModel>);
    }

    extern "RustQt" {
        #[qinvokable]
        fn load(self: Pin<&mut ChatModel>, fixture_dir: &QString) -> bool;
        #[qinvokable]
        #[cxx_override]
        fn data(self: &ChatModel, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &ChatModel) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &ChatModel, parent: &QModelIndex) -> i32;

        #[qinvokable]
        fn load(self: Pin<&mut RosterModel>, fixture_dir: &QString) -> bool;
        #[qinvokable]
        #[cxx_override]
        fn data(self: &RosterModel, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &RosterModel) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &RosterModel, parent: &QModelIndex) -> i32;

        #[qinvokable]
        fn load(self: Pin<&mut ChannelModel>, fixture_dir: &QString) -> bool;
        #[qinvokable]
        #[cxx_override]
        fn data(self: &ChannelModel, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &ChannelModel) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &ChannelModel, parent: &QModelIndex) -> i32;
    }
}

// ── the fixture, parsed once per process ──────────────────────────────────────────────────────────────
static FIXTURE: OnceLock<Result<(Fixture, PathBuf), String>> = OnceLock::new();

fn fixture(dir: &QString) -> Option<&'static (Fixture, PathBuf)> {
    let r = FIXTURE.get_or_init(|| {
        let dir = PathBuf::from(dir.to_string());
        fixture::load(&dir.join("chat.json")).map(|f| (f, dir))
    });
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("fixture: {e}");
            None
        }
    }
}

fn roles(names: &[&str]) -> QHash<QHashPair_i32_QByteArray> {
    let mut h = QHash::<QHashPair_i32_QByteArray>::default();
    for (i, n) in names.iter().enumerate() {
        h.insert(0x100 + i as i32, QByteArray::from(*n));
    }
    h
}

fn q(s: &str) -> QString {
    QString::from(s)
}

// ── messages ─────────────────────────────────────────────────────────────────────────────────────────
const CHAT_ROLES: &[&str] = &[
    "messageId", "senderName", "colorIndex", "avatar", "initial", "timeLabel", "dayLabel", "grouped", "body",
    "codeLang", "codeText", "hasReply", "replyName", "replyColor", "replySnippet", "imageCount", "image0",
    "image0W", "image0H", "image1", "image1W", "image1H", "image2", "image2W", "image2H",
];

pub struct ChatRow {
    id: QString,
    sender: QString,
    color: i32,
    avatar: QString,
    initial: QString,
    time: QString,
    day: QString,
    grouped: bool,
    body: QString,
    code_lang: QString,
    code_text: QString,
    reply: Option<(QString, i32, QString)>,
    images: Vec<(QString, i32, i32)>,
}

#[derive(Default)]
pub struct ChatModelRust {
    rows: Vec<ChatRow>,
    count: i32,
    server_name: QString,
}

impl qobject::ChatModel {
    pub fn load(mut self: Pin<&mut Self>, fixture_dir: &QString) -> bool {
        let Some((f, dir)) = fixture(fixture_dir) else { return false };
        // Newest first: the list runs bottom-to-top, so the newest row sits at the bottom with no positioning,
        // and older history (a "prepend" on screen) is an append at the end of the model: nothing moves.
        let rows: Vec<ChatRow> = fixture::rows(f, Path::new(dir))
            .into_iter()
            .rev()
            .map(|r| {
                let (body, code) = fixture::styled_body(&r.content);
                let (code_lang, code_text) = code.unwrap_or_default();
                ChatRow {
                    id: q(&r.id),
                    sender: q(&r.sender_name),
                    color: r.color_index as i32,
                    avatar: q(&r.avatar),
                    initial: q(&r.initial),
                    time: q(&r.time_label),
                    day: q(&r.day_label),
                    grouped: r.grouped,
                    body: q(&body),
                    code_lang: q(&code_lang),
                    code_text: q(&code_text),
                    reply: r.reply.map(|x| (q(&x.sender_name), x.color_index as i32, q(&x.snippet))),
                    images: r.images.iter().map(|a| (q(&a.file), a.width as i32, a.height as i32)).collect(),
                }
            })
            .collect();
        unsafe { self.as_mut().begin_reset_model() };
        {
            let mut me = self.as_mut().rust_mut();
            me.count = rows.len() as i32;
            me.rows = rows;
        }
        unsafe { self.as_mut().end_reset_model() };
        let n = self.rows.len() as i32;
        self.as_mut().set_count(n);
        self.as_mut().set_server_name(q(&f.server.name));
        true
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(r) = self.rows.get(index.row() as usize) else { return QVariant::default() };
        let img = |i: usize, part: u8| -> QVariant {
            match (r.images.get(i), part) {
                (Some(x), 0) => QVariant::from(&x.0),
                (Some(x), 1) => QVariant::from(&x.1),
                (Some(x), _) => QVariant::from(&x.2),
                (None, 0) => QVariant::from(&QString::default()),
                (None, _) => QVariant::from(&0i32),
            }
        };
        match role - 0x100 {
            0 => QVariant::from(&r.id),
            1 => QVariant::from(&r.sender),
            2 => QVariant::from(&r.color),
            3 => QVariant::from(&r.avatar),
            4 => QVariant::from(&r.initial),
            5 => QVariant::from(&r.time),
            6 => QVariant::from(&r.day),
            7 => QVariant::from(&r.grouped),
            8 => QVariant::from(&r.body),
            9 => QVariant::from(&r.code_lang),
            10 => QVariant::from(&r.code_text),
            11 => QVariant::from(&r.reply.is_some()),
            12 => r.reply.as_ref().map_or_else(|| QVariant::from(&QString::default()), |x| QVariant::from(&x.0)),
            13 => QVariant::from(&r.reply.as_ref().map_or(0, |x| x.1)),
            14 => r.reply.as_ref().map_or_else(|| QVariant::from(&QString::default()), |x| QVariant::from(&x.2)),
            15 => QVariant::from(&(r.images.len() as i32)),
            n @ 16..=24 => img(((n - 16) / 3) as usize, ((n - 16) % 3) as u8),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        roles(CHAT_ROLES)
    }

    pub fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }
}

// ── roster: section rows ("Online — N") then members, by the community's online view ────────────────
const ROSTER_ROLES: &[&str] = &["isSection", "label", "name", "colorIndex", "avatar", "initial", "status", "online"];

pub struct RosterRow {
    section: bool,
    label: QString,
    name: QString,
    color: i32,
    avatar: QString,
    initial: QString,
    status: QString,
    online: bool,
}

#[derive(Default)]
pub struct RosterModelRust {
    rows: Vec<RosterRow>,
}

impl qobject::RosterModel {
    pub fn load(mut self: Pin<&mut Self>, fixture_dir: &QString) -> bool {
        let Some((f, dir)) = fixture(fixture_dir) else { return false };
        let url = |rel: &str| format!("file:///{}", dir.join(rel).to_string_lossy().replace('\\', "/"));
        let mut members = f.members.clone();
        members.sort_by_key(|m| m.name.to_lowercase());
        let mut rows = Vec::new();
        for online in [true, false] {
            let group: Vec<_> = members.iter().filter(|m| m.online == online).collect();
            if group.is_empty() {
                continue;
            }
            rows.push(RosterRow {
                section: true,
                label: q(&format!("{} — {}", if online { "Online" } else { "Offline" }, group.len())),
                name: QString::default(),
                color: 0,
                avatar: QString::default(),
                initial: QString::default(),
                status: QString::default(),
                online,
            });
            for m in group {
                rows.push(RosterRow {
                    section: false,
                    label: QString::default(),
                    name: q(&m.name),
                    color: fixture::color_index(m.uid, &m.name) as i32,
                    avatar: q(&m.avatar.as_deref().map(url).unwrap_or_default()),
                    initial: q(&m.name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()),
                    status: q(&m.status),
                    online,
                });
            }
        }
        unsafe { self.as_mut().begin_reset_model() };
        self.as_mut().rust_mut().rows = rows;
        unsafe { self.as_mut().end_reset_model() };
        true
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(r) = self.rows.get(index.row() as usize) else { return QVariant::default() };
        match role - 0x100 {
            0 => QVariant::from(&r.section),
            1 => QVariant::from(&r.label),
            2 => QVariant::from(&r.name),
            3 => QVariant::from(&r.color),
            4 => QVariant::from(&r.avatar),
            5 => QVariant::from(&r.initial),
            6 => QVariant::from(&r.status),
            7 => QVariant::from(&r.online),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        roles(ROSTER_ROLES)
    }

    pub fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }
}

// ── channels: one flat ordered list, categories as rows (a channel's category is the one above it) ──
const CHANNEL_ROLES: &[&str] = &["channelId", "name", "kind", "encrypted"];

pub struct ChannelRow {
    id: QString,
    name: QString,
    kind: QString,
    encrypted: bool,
}

#[derive(Default)]
pub struct ChannelModelRust {
    rows: Vec<ChannelRow>,
}

impl qobject::ChannelModel {
    pub fn load(mut self: Pin<&mut Self>, fixture_dir: &QString) -> bool {
        let Some((f, _)) = fixture(fixture_dir) else { return false };
        let rows = f
            .channels
            .iter()
            .map(|c| ChannelRow { id: q(&c.id), name: q(&c.name), kind: q(&c.kind), encrypted: c.encrypted })
            .collect();
        unsafe { self.as_mut().begin_reset_model() };
        self.as_mut().rust_mut().rows = rows;
        unsafe { self.as_mut().end_reset_model() };
        true
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(r) = self.rows.get(index.row() as usize) else { return QVariant::default() };
        match role - 0x100 {
            0 => QVariant::from(&r.id),
            1 => QVariant::from(&r.name),
            2 => QVariant::from(&r.kind),
            3 => QVariant::from(&r.encrypted),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        roles(CHANNEL_ROLES)
    }

    pub fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }
}
