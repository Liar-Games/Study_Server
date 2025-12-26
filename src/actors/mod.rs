
pub mod messages;
pub mod client;
pub mod session_manager;
pub mod room_manager;
pub mod rooms;



// TODO : need constants.rs ??
pub use self::rooms::RoomType;

pub use self::packet_tag::PacketTag;

mod packet_tag {
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PacketTag {
        Heartbeat = 0,
        Auth = 1,
        Text = 2,
        TextError = 3,
        Binary = 4,
        Game = 10,
    }
    
    impl PacketTag {
        pub fn from_u8(value: u8) -> Option<Self> {
            match value {
                0 => Some(Self::Heartbeat),
                1 => Some(Self::Auth),
                2 => Some(Self::Text),
                3 => Some(Self::TextError),
                4 => Some(Self::Binary),
                10 => Some(Self::Game),
                _ => None,
            }
        }
    }
}