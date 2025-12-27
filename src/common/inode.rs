use glib::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use mpd::{Playlist, lsinfo::LsInfoEntry};
use std::cell::OnceCell;

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaINodeType")]
pub enum INodeType {
    #[default]
    Unknown, // Catch-all
    Song,
    Folder,
    Playlist,
    DynamicPlaylist,
}

impl INodeType {
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Folder => "folder-symbolic",
            Self::Song => "music-note-single-symbolic",
            Self::Playlist | Self::DynamicPlaylist => "playlist-symbolic",
            _ => "paper-symbolic",
        }
    }
}

//  TODO: more detailed fields
#[derive(Debug, Clone, PartialEq)]
pub struct INodeInfo {
    pub uri: String,
    pub last_modified: Option<String>,
    pub inode_type: INodeType,
}

impl INodeInfo {
    pub fn new(uri: &str, last_modified: Option<&str>, inode_type: INodeType) -> Self {
        Self {
            uri: uri.to_owned(),
            last_modified: last_modified.map(String::from),
            inode_type,
        }
    }
}

impl Default for INodeInfo {
    fn default() -> Self {
        INodeInfo {
            uri: "".to_owned(),
            last_modified: None,
            inode_type: INodeType::default(),
        }
    }
}

impl From<LsInfoEntry> for INodeInfo {
    fn from(entry: LsInfoEntry) -> Self {
        match entry {
            LsInfoEntry::Song(song) => Self {
                uri: song.file,
                last_modified: song.last_mod,
                inode_type: INodeType::Song,
            },
            LsInfoEntry::Directory(dir) => Self {
                uri: dir.name,
                last_modified: dir.last_mod,
                inode_type: INodeType::Folder,
            },
        }
    }
}

impl From<Playlist> for INodeInfo {
    fn from(playlist: Playlist) -> Self {
        Self {
            uri: playlist.name,
            last_modified: Some(playlist.last_mod),
            inode_type: INodeType::Playlist,
        }
    }
}

mod imp {
    use super::*;
    use glib::{ParamSpec, ParamSpecEnum, ParamSpecString};
    use once_cell::sync::Lazy;

    /// The GObject Song wrapper.
    /// By nesting info inside another struct, we enforce tag editing to be
    /// atomic. Tag editing is performed by first cloning the whole SongInfo
    /// struct to a mutable variable, modify it, then create a new Song wrapper
    /// from the modified SongInfo struct (no copy required this time).
    /// This design also avoids a RefCell.
    #[derive(Default, Debug)]
    pub struct INode {
        pub info: OnceCell<INodeInfo>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for INode {
        const NAME: &'static str = "EuphonicaINode";
        type Type = super::INode;

        fn new() -> Self {
            Self {
                info: OnceCell::new(),
            }
        }
    }

    impl ObjectImpl for INode {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("last-modified")
                        .read_only()
                        .build(),
                    ParamSpecEnum::builder::<INodeType>("inode-type").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let info = self.info.get().unwrap();
            match pspec.name() {
                "uri" => info.uri.to_value(),
                "last-modified" => info.last_modified.to_value(),
                "inode-type" => info.inode_type.to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct INode(ObjectSubclass<imp::INode>);
}

impl INode {
    // ALL of the getters below require that the info field be initialised!
    pub fn get_info(&self) -> &INodeInfo {
        self.imp().info.get().unwrap()
    }

    pub fn get_uri(&self) -> &str {
        &self.get_info().uri
    }

    /// Get the last part of the URI
    pub fn get_name(&self) -> Option<&str> {
        self.get_info().uri.split("/").last()
    }

    /// Set a new name for this INode. Caution: only to be used with playlists.
    pub fn with_new_name(&self, new_name: &str) -> Self {
        let old_info = self.get_info();
        Self::from(INodeInfo {
            uri: new_name.to_owned(),
            last_modified: old_info.last_modified.clone(),
            inode_type: old_info.inode_type,
        })
    }

    pub fn get_last_modified(&self) -> Option<&str> {
        self.get_info().last_modified.as_deref()
    }
}

impl Default for INode {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<INodeInfo> for INode {
    fn from(info: INodeInfo) -> Self {
        let res = glib::Object::builder::<Self>().build();
        let _ = res.imp().info.set(info);
        res
    }
}

impl From<LsInfoEntry> for INode {
    fn from(entry: LsInfoEntry) -> Self {
        let info = INodeInfo::from(entry);
        Self::from(info)
    }
}

impl From<Playlist> for INode {
    fn from(playlist: Playlist) -> Self {
        let info = INodeInfo::from(playlist);
        Self::from(info)
    }
}
