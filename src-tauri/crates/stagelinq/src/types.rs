/// SoundSwitch token used for device identification.
pub const SOUNDSWITCH_TOKEN: [u8; 16] = [
    0x52, 0xFD, 0xFC, 0x07, 0x21, 0x82, 0x65, 0x4F, 0x16, 0x3F, 0x5F, 0x0F, 0x9A, 0x62, 0x1D, 0x72,
];

/// UDP discovery port.
pub const DISCOVERY_PORT: u16 = 51337;

/// "airD" marker for discovery messages.
pub const DISCOVERY_MARKER: &[u8; 4] = b"airD";

/// "smaa" marker for StateMap messages.
pub const STATEMAP_MARKER: &[u8; 4] = b"smaa";

/// StateMap message type: JSON value update.
pub const STATEMAP_TYPE_JSON: u32 = 0x0000_0000;

/// StateMap message type: interval / subscription.
pub const STATEMAP_TYPE_INTERVAL: u32 = 0x0000_07D2;

/// Discovery action strings.
pub const ACTION_LOGIN: &str = "DISCOVERER_HOWDY_";
pub const ACTION_LOGOUT: &str = "DISCOVERER_EXIT_";

/// TCP message IDs on the main device connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MessageId {
    ServicesAnnouncement = 0,
    TimeStamp = 1,
    ServicesRequest = 2,
}

impl MessageId {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::ServicesAnnouncement),
            1 => Some(Self::TimeStamp),
            2 => Some(Self::ServicesRequest),
            _ => None,
        }
    }
}

/// Our software identity when announcing on the network.
/// Must match a known identity that Denon hardware recognizes.
/// Using "nowplaying" / "np2" which is the SoundSwitch NowPlaying client identity.
pub const SOFTWARE_NAME: &str = "nowplaying";
pub const SOFTWARE_VERSION: &str = "2.2.0";
pub const SOFTWARE_SOURCE: &str = "np2";

/// Timeouts.
pub const ANNOUNCEMENT_INTERVAL_MS: u64 = 1000;
pub const CONNECT_TIMEOUT_MS: u64 = 5000;

/// Service names we look for.
pub const SERVICE_STATE_MAP: &str = "StateMap";
pub const SERVICE_BEAT_INFO: &str = "BeatInfo";

/// State paths to subscribe to (per deck N = 1..4).
pub fn deck_state_paths(deck: u8) -> Vec<String> {
    let n = deck;
    vec![
        format!("/Engine/Deck{n}/Play"),
        format!("/Engine/Deck{n}/CurrentBPM"),
        format!("/Engine/Deck{n}/ExternalMixerVolume"),
        format!("/Engine/Deck{n}/Track/SongName"),
        format!("/Engine/Deck{n}/Track/ArtistName"),
        format!("/Engine/Deck{n}/Track/SongLoaded"),
        format!("/Engine/Deck{n}/Track/TrackNetworkPath"),
        format!("/Engine/Deck{n}/Track/TrackLength"),
        format!("/Engine/Deck{n}/Track/SampleRate"),
        format!("/Engine/Deck{n}/DeckIsMaster"),
        format!("/Engine/Deck{n}/Track/SoundSwitchGuid"),
        format!("/Engine/Deck{n}/Track/TrackUri"),
    ]
}

/// Mixer state paths.
pub fn mixer_state_paths() -> Vec<String> {
    let mut paths = Vec::new();
    for ch in 1..=4 {
        paths.push(format!("/Mixer/CH{ch}faderPosition"));
    }
    paths.push("/Mixer/CrossfaderPosition".to_string());
    paths.push("/Engine/Master/MasterTempo".to_string());
    paths
}
