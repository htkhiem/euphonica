use chrono::{DateTime, Utc};

// "LikeStatus" sounded obtuse so have this instead
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thumbs {
    Up,
    #[default]
    Sideways,
    Down,
}

impl TryFrom<i8> for Thumbs {
    type Error = ();
    fn try_from(value: i8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Down),
            1 => Ok(Self::Sideways),
            2 => Ok(Self::Up),
            _ => Err(()),
        }
    }
}

// Our sticker schema
// Largely follows myMPD's schema
#[derive(Default, Debug, Clone)]
pub struct Stickers {
    pub rating: Option<i8>,
    pub like: Thumbs,                        // 0 = dislike, 1 = neutral, 2 = like
    pub elapsed: Option<i64>,                // in seconds
    pub last_played: Option<DateTime<Utc>>,  // Unix timestamp
    pub last_skipped: Option<DateTime<Utc>>, // Unix timestamp
    pub play_count: Option<i64>,             // use myMPD rules
    pub skip_count: Option<i64>,             // use myMPD rules
}

impl Stickers {
    pub const RATING_KEY: &'static str = "rating";
    pub const LIKE_KEY: &'static str = "like";
    pub const ELAPSED_KEY: &'static str = "elapsed";
    pub const LAST_PLAYED_KEY: &'static str = "lastPlayed";
    pub const LAST_SKIPPED_KEY: &'static str = "lastSkipped";
    pub const PLAY_COUNT_KEY: &'static str = "playCount";
    pub const SKIP_COUNT_KEY: &'static str = "skipCount";

    pub fn from_mpd_kv(kvs: Vec<(String, String)>) -> Self {
        let mut res = Self::default();
        for kv in kvs.iter() {
            let val = kv.1.as_str();
            match kv.0.as_str() {
                Self::RATING_KEY => {
                    res.set_rating(val);
                }
                Self::LIKE_KEY => {
                    res.set_like(val);
                }
                Self::ELAPSED_KEY => {
                    res.set_elapsed(val);
                }
                Self::LAST_PLAYED_KEY => {
                    res.set_last_played(val);
                }
                Self::LAST_SKIPPED_KEY => {
                    res.set_last_skipped(val);
                }
                Self::PLAY_COUNT_KEY => {
                    res.set_play_count(val);
                }
                Self::SKIP_COUNT_KEY => {
                    res.set_skip_count(val);
                }
                _ => {}
            }
        }

        res
    }

    pub fn set_rating(&mut self, val: &str) {
        if let Ok(rating) = val.trim().parse::<i8>() {
            self.rating = Some(rating);
        }
    }

    pub fn set_like(&mut self, val: &str) {
        if let Ok(Ok(status)) = val.trim().parse::<i8>().map(Thumbs::try_from) {
            self.like = status;
        }
    }

    pub fn set_elapsed(&mut self, val: &str) {
        if let Ok(elapsed) = val.trim().parse::<i64>() {
            self.elapsed = Some(elapsed);
        }
    }

    pub fn set_last_played(&mut self, val: &str) {
        if let Ok(maybe_dt) = val
            .trim()
            .parse::<i64>()
            .map(|unix_ts| DateTime::from_timestamp(unix_ts, 0))
        {
            self.last_played = maybe_dt;
        }
    }

    pub fn set_last_skipped(&mut self, val: &str) {
        if let Ok(maybe_dt) = val
            .trim()
            .parse::<i64>()
            .map(|unix_ts| DateTime::from_timestamp(unix_ts, 0))
        {
            self.last_skipped = maybe_dt;
        }
    }

    pub fn set_play_count(&mut self, val: &str) {
        if let Ok(count) = val.trim().parse::<i64>() {
            self.play_count = Some(count);
        }
    }

    pub fn set_skip_count(&mut self, val: &str) {
        if let Ok(count) = val.trim().parse::<i64>() {
            self.skip_count = Some(count);
        }
    }
}
