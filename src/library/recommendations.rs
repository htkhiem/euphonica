/*
Lightweight recommender algorithms. No don't expect anything Sp*tify-level here. These run
inside SQLite, only uses your CPU, and cannot actually listen to the songs themselves (operating
purely on history and metadata).
They do feel fuzzy enough though :).
 */
use rusqlite::params;

use crate::cache::sqlite::{SQLITE_POOL, Error};

/// Get albums and artists worth "rediscovering": oft-listened, but not so lately.
/// Albums are returned as (title, artist, mbid), while artists are returned by name.
pub fn rediscover(n: u32) -> Result<(Vec<(String, Option<String>, Option<String>)>, Vec<String>), Error> {
    let conn = SQLITE_POOL.get().unwrap();

    // Albums: score by affinity * staleness, where affinity is ln(1 + listens count) and
    // staleness is number of days since the last time any track from this album was played ^ 0.4.
    // To be eligible, the last play must have been at least 3mo ago.
    let mut albums_query = conn
        .prepare(
            "
select title, artist, mbid, ln(1 + listen_count) * pow(n_days_since_last_play, 0.4) as rediscover_score from (
	select title, artist, mbid, (julianday('now') - julianday(max(timestamp))) as n_days_since_last_play, count(*) as listen_count from albums_history group by title, artist, mbid
) where n_days_since_last_play > 90 order by rediscover_score desc limit ?1;"
        )
        .unwrap();
    let albums = albums_query
        .query_map(params![n], |r| {
            Ok((
                r.get::<usize, String>(0)?,
                r.get::<usize, Option<String>>(1)?,
                r.get::<usize, Option<String>>(2)?,
            ))
        })
        .map_err(Error::Db)?
        .map(|r| r.unwrap())
        .collect();

    // Artists: for now same formula as above.
    // The behaviour will still be a bit different: this will suggest artists
    // based on the LAST PLAY OF ANY TRACK FROM ANY ALBUM, effectively ranking
    // artists whose ENTIRE catalog haven't been touched lately top. To discover
    // their "back-catalog", refer to the above albums instead?
    let mut artists_query = conn
        .prepare(
            "
select name, ln(1 + listen_count) * pow(n_days_since_last_play, 0.4) as rediscover_score from (
	select name, (julianday('now') - julianday(max(timestamp))) as n_days_since_last_play, count(*) as listen_count from artists_history group by name
) where n_days_since_last_play > 90 order by rediscover_score desc limit ?1;"
        )
        .unwrap();
    let artists = artists_query
        .query_map(params![n], |r| {
            Ok(
                r.get::<usize, String>(0)?
            )
        })
        .map_err(Error::Db)?
        .map(|r| r.unwrap())
        .collect();

    Ok((albums, artists))
}