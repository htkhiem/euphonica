/* search_provider.rs
 *
 * Copyright 2026 htkhiem2000
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_channel::Sender;
use gtk::glib::{self, clone};
use mpd::search::{Operation, Window};
use mpd::{Query, Term, song::Song as SongInfo};
use search_provider::{
    ResultID, ResultMeta, ResultMetaBuilder, SearchProvider, SearchProviderImpl,
};

use crate::common::tags;
use crate::{
    application::EuphonicaApplication,
    client::{MpdWrapper, Task},
    common::song::get_title_from_mpd_song,
    utils::tokio_runtime,
};

/// The shell truncates anyway, but don't ask MPD for more than we need
const MAX_RESULTS: u32 = 20;
/// Upper bound on how long a background-client task may wait for a response
/// before being considered wedged (e.g. the client thread died mid-request).
const MPD_TASK_TIMEOUT: Duration = Duration::from_secs(5);

/// GNOME Shell search provider. Registered on the session bus for
/// as long as the app runs. Having the shell starts us is a bit silly since
/// we're meant to run in the background anyway, so autostart will be false.
pub struct EuphonicaSearchProvider {
    fg: Sender<Task>,
    // For now the only thing the provider does with the main thread is popping
    // the window open. Might add navigation to queue or content view later.
    main_thread_sender: Sender<()>,
    /// Songs backing the most recent result set, keyed by URI (= ResultID)
    song_cache: Mutex<HashMap<String, SongInfo>>,
}

/// Register the search provider on the session bus and start pumping it.
/// Must be called from the main thread exactly once, after the MpdWrapper
/// has been created.
pub fn start(client: &MpdWrapper, app: &EuphonicaApplication) {
    let fg = client.fg_sender();
    let (main_thread_send, main_thread_recv) = async_channel::bounded(1);

    glib::spawn_future_local(clone!(
        #[weak]
        app,
        async move {
            while let Ok(()) = main_thread_recv.recv().await {
                app.raise_window();
            }
        }
    ));

    let imp = EuphonicaSearchProvider {
        fg,
        main_thread_sender: main_thread_send,
        song_cache: Mutex::default(),
    };

    // Actual run loop for the search provider. Since our application struct
    // is heavily async we'll have the provider run separately from it.
    tokio_runtime().spawn(async move {
        let _provider = SearchProvider::new(
            imp,
            String::from("io.github.htkhiem.Euphonica.SearchProvider"),
            String::from(
                "/org/gnome/Shell/SearchProvider/io/github/htkhiem/Euphonica/SearchProvider",
            ),
        )
        .await;
    });
}

/// The crate's trait methods are synchronous but execute inside a tokio task,
/// where `Runtime::block_on` would panic. Hop to a scoped thread (which has no
/// runtime context) to run the async MPD work to completion.
fn block_on_mpd<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(move || {
            let out = tokio_runtime().block_on(fut);
            let _ = tx.send(out);
        });
    });
    rx.recv().expect("MPD bridge thread died")
}

/// Build the result description from a song's album & artist tags:
/// "in <album> by <artist1, artist2, ...>", degrading gracefully when
/// either tag is missing.
fn song_description(song: &mpd::Song) -> Option<String> {
    let album = song.album.as_deref();
    let artists = if song.artists.is_empty() {
        None
    } else {
        Some(song.artists.join(", "))
    };
    match (album, artists) {
        (Some(album), Some(artists)) => Some(format!("in {album} by {artists}")),
        (None, Some(artists)) => Some(format!("by {artists}")),
        (Some(album), None) => Some(format!("in {album}")),
        (None, None) => None,
    }
}

impl SearchProviderImpl for EuphonicaSearchProvider {
    /// We only search by title for now, and phrases don't match at all,
    /// so only use the first non-empty term.
    /// The "ID" given to GNOME is the full song URI.
    fn initial_result_set(&self, terms: &[String]) -> Vec<ResultID> {
        let term = match terms.iter().map(|t| t.trim()).find(|t| !t.is_empty()) {
            Some(term) => term.to_owned(),
            None => return Vec::with_capacity(0),
        };

        block_on_mpd(async {
            let (sender, receiver) = oneshot::channel();
            let mut query = Query::new();
            // Case-insensitive
            query.and_with_op(Term::Tag("title".into()), Operation::Contains, term);
            // We only need the URIs right now so set tagtypes to none.
            // GNOME will come back asking for details later lol.
            self.fg
                .send(Task::Find(
                    query,
                    Window::from((0, MAX_RESULTS)),
                    Some(Vec::with_capacity(0)),
                    sender,
                ))
                .await
                .expect("Broken FG sender");
            // The oneshot crate's receiver future is itself a Result (Err if
            // the sender was dropped without responding), hence the triple nest
            match tokio::time::timeout(MPD_TASK_TIMEOUT, receiver).await {
                Ok(Ok(Ok(songs))) => songs.into_iter().map(|song| song.file).collect(),
                Ok(Ok(Err(e))) => {
                    eprintln!("Search provider search failed: {e:?}");
                    vec![]
                }
                Ok(Err(e)) => {
                    eprintln!("Search provider: MPD task channel dropped: {e:?}");
                    vec![]
                }
                Err(_) => {
                    eprintln!("Search provider search timed out");
                    vec![]
                }
            }
        })
    }

    /// Fetch again here, this time with more tags on the (usually) more limited ID set.
    /// "Usually" since last I checked GNOME only asks for ~3 of the initial result set.
    fn result_metas(&self, identifiers: &[ResultID]) -> Vec<ResultMeta> {
        block_on_mpd(async {
            let (sender, receiver) = oneshot::channel();
            let queries_windows = identifiers
                .iter()
                .map(|uri| {
                    let mut q = Query::new();
                    q.and(Term::File, Cow::Owned(uri.to_owned()));
                    (q, Window::from((0, 1)))
                })
                .collect::<Vec<(Query, Window)>>();
            self.fg
                .send(Task::FindMultiple(
                    queries_windows,
                    Some(vec![tags::ARTIST, tags::ALBUM, tags::TITLE]),
                    sender,
                ))
                .await
                .expect("Broken FG sender");
            // As above, dumb triple nested res
            match tokio::time::timeout(MPD_TASK_TIMEOUT, receiver).await {
                Ok(Ok(Ok(songs))) => songs
                    .into_iter()
                    .map(|s| {
                        let mut res =
                            ResultMetaBuilder::new(s.file.to_owned(), &get_title_from_mpd_song(&s));
                        if let Some(desc) = song_description(&s) {
                            res = res.description(&desc);
                        }
                        res.build()
                    })
                    .collect(),
                Ok(Ok(Err(e))) => {
                    eprintln!("Search provider search failed: {e:?}");
                    vec![]
                }
                Ok(Err(e)) => {
                    eprintln!("Search provider: MPD task channel dropped: {e:?}");
                    vec![]
                }
                Err(_) => {
                    eprintln!("Search provider search timed out");
                    vec![]
                }
            }
        })
    }

    fn activate_result(&self, identifier: ResultID, _terms: &[String], _timestamp: u32) {
        let fg = self.fg.clone();
        let main_thread_sender = self.main_thread_sender.clone();
        // Fire-and-forget: the shell doesn't wait for anything meaningful here
        tokio_runtime().spawn(async move {
            // Append the song to the end of the queue, then start playing it.
            // Trigger by queue ID instead so we don't need to get the queue length first.
            let (sender, receiver) = oneshot::channel();
            fg.send(Task::Add(identifier, sender))
                .await
                .expect("Broken FG sender");
            // As above, dumb triple nested res
            if let Ok(Ok(Ok(id))) = tokio::time::timeout(MPD_TASK_TIMEOUT, receiver).await {
                let (sender, receiver) = oneshot::channel();
                fg.send(Task::PlayAtId(id, sender))
                    .await
                    .expect("Broken FG sender");
                let _ = tokio::time::timeout(MPD_TASK_TIMEOUT, receiver).await;
            };
            // Bring the app to the front so the new current song is visible
            let _ = main_thread_sender.try_send(());
        });
    }

    fn launch_search(&self, _terms: &[String], _timestamp: u32) {
        // No in-app search view yet, so just focus the app
        let _ = self.main_thread_sender.try_send(());
    }
}