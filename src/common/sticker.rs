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
    // myMPD-compatible stickers
    pub const RATING: &'static str = "rating";  // Usage with non-song entities is now deprecated
    pub const LIKE: &'static str = "like";
    pub const ELAPSED: &'static str = "elapsed";
    pub const LAST_PLAYED: &'static str = "lastPlayed";
    pub const LAST_SKIPPED: &'static str = "lastSkipped";
    pub const PLAY_COUNT: &'static str = "playCount";
    pub const SKIP_COUNT: &'static str = "skipCount";

    pub const COMMON_NAMES: &'static [&'static str] = &[
        Self::RATING,
        Self::LIKE,
        Self::ELAPSED,
        Self::LAST_PLAYED,
        Self::LAST_SKIPPED,
        Self::PLAY_COUNT,
        Self::SKIP_COUNT,
    ];

    // Reserved for Euphonica-specific features (prefixed with our names to avoid collisions).
    // Not part of the "common" set as its value is complex and probably too specific to our app.
    // Open an issue if you think these may be of use for other clients too (and would like the
    // name prefix dropped, for example).
    pub const META_DOC: &'static str = "euphonica:meta:doc";
    pub const META_LAST_MODIFIED: &'static str = "euphonica:meta:lastModified";
    pub const META_PAGE_COUNT: &'static str = "euphonica:meta:pageCount";  // starts from 0

    pub fn from_mpd_kv(kvs: Vec<(String, String)>) -> Self {
        let mut res = Self::default();
        for kv in kvs.iter() {
            let val = kv.1.as_str();
            match kv.0.as_str() {
                Self::RATING => {
                    res.set_rating(val);
                }
                Self::LIKE => {
                    res.set_like(val);
                }
                Self::ELAPSED => {
                    res.set_elapsed(val);
                }
                Self::LAST_PLAYED => {
                    res.set_last_played(val);
                }
                Self::LAST_SKIPPED => {
                    res.set_last_skipped(val);
                }
                Self::PLAY_COUNT => {
                    res.set_play_count(val);
                }
                Self::SKIP_COUNT => {
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

    /// Convert this Stickers struct back into a list of (name, value) pairs.
    pub fn into_mpd_kv(self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        if let Some(rating) = self.rating {
            result.push((Self::RATING.to_string(), rating.to_string()));
        }
        result.push((Self::LIKE.to_string(), (self.like as i8).to_string()));
        if let Some(elapsed) = self.elapsed {
            result.push((Self::ELAPSED.to_string(), elapsed.to_string()));
        }
        if let Some(dt) = self.last_played {
            result.push((Self::LAST_PLAYED.to_string(), dt.timestamp().to_string()));
        }
        if let Some(dt) = self.last_skipped {
            result.push((Self::LAST_SKIPPED.to_string(), dt.timestamp().to_string()));
        }
        if let Some(count) = self.play_count {
            result.push((Self::PLAY_COUNT.to_string(), count.to_string()));
        }
        if let Some(count) = self.skip_count {
            result.push((Self::SKIP_COUNT.to_string(), count.to_string()));
        }
        result
    }
}
