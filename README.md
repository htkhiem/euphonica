![Euphonica icon](data/icons/hicolor/scalable/apps/io.github.htkhiem.Euphonica.svg)
# Euphonica

An MPD frontend with delusions of grandeur. 

## Features
- Responsive GTK4 LibAdwaita UI for most MPD features, from basic things like playback controls, queue reordering and ReplayGain to things like output control, crossfade and MixRamp configuration
- Built-in, customisable spectrum visualiser, reading from MPD FIFO or system PipeWire
- Automatic accent colours based on album art (optional)
- Rate albums (requires MPD 0.24+)
- Audio quality indicators (lossy, lossless, hi-res, DSD) for individual songs as well as albums & detailed format printout
- Browse your library by album, artist and folders with multiselection support
  - Browsing by genre and other criteria are planned.
- Playlist browser and editor:
  - Save current queue as playlist
  - Create playlists from selected songs or append to existing ones
  - Rename existing playlists + reorder or remove songs in them
- Sort albums by name, AlbumArtist or release date (provided you have the tags)
- Asynchronous search for large collections
- Configurable multi-artist tag syntax, works with anything you throw at it
  - In other words, your artist tags can be pretty messy and Euphonica will still be able to correctly split them into individual artists.
- Performant album art fetching & display (cached with Stretto)
- Super-fast, **multithreaded**, **statically-cached** background blur powered by [libblur](https://github.com/awxkee/libblur)'s stack blur implementation.
  - Completely independent of blur radius in terms of time complexity.
  - Multithreaded, queued update logic never blocks UI and **only runs when needed** (once _after_ window resizes, once every time album art changes, etc).
- Automatically fetch album arts & artist avatars from external sources (currently supports Last.fm and MusicBrainz)
- Album wikis & artist bios are supported too
- All externally-acquired metadata are cached locally & persisted on disk to avoid needless API calls
- Volume knob with dBFS readout support ('cuz why not?)
- MPRIS support (can be disabled if you're running `mpdris2` instead)
- User-friendly configuration UI & GSettings backend
- MPD passwords are securely stored in your user's login keyring
- Commands are bundled into lists for efficient MPD-side processing where possible.
- Written in Rust so my dumb code can still be quick :)

## Screenshots

The below were captured with a mix of dark and light modes.

- Album View[^1]
  ![album-view](https://github.com/user-attachments/assets/26f9f3bb-3032-4ae5-ba46-15e4ece680ef)

- UI at different sizes (v0.12+)[^1]
  ![mini-layouts-v2](https://github.com/user-attachments/assets/1caace0d-751e-41c6-be9c-5aa54ac67f91)

- Queue View[^1]
  ![queue-view](https://github.com/user-attachments/assets/020faf32-33f3-4711-a86c-4934d058f3a1)

- Artist bio as fetched from Last.fm[^1][^2][^3]
  ![artist-content-view](https://github.com/user-attachments/assets/54161399-1f16-490f-91b9-89b581b28839)

- Album wiki as fetched from Last.fm[^1][^2]
  ![album-content-view](https://github.com/user-attachments/assets/f3246730-3ad1-4056-a414-37807a145dc2)
  
- Playlist Content View[^1]
  ![playlist-content-view](https://github.com/user-attachments/assets/be9913e7-2378-4374-9a8a-d08512fc1e09)
  
- Some of the available UI customisations[^1]
  ![visualiser-customisation](https://github.com/user-attachments/assets/e21d0a36-6dba-4fea-84a2-0fdb0810bff5)

[^1]: Actual album arts have been replaced with random pictures from [Pexels](https://www.pexels.com/). All credits go to the original authors.
[^2]: Artist bios and album wikis are user-contributed and licensed by Last.fm under CC-BY-SA.
[^3]: The displayed image has been released into the public domain. More information at [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Johann_Sebastian_Bach.jpg).

## Installation

Euphonica is still in very early development, and so far has only been tested on Arch Linux (btw).

I'm gearing up for AUR & Flathub releases, but before that happens, Euphonica must be built from source.

### Using `flatpak-builder`

This builds and installs Euphonica as a sandboxed Flatpak app on your system, complete with an entry in 
Flatpak-aware app stores (like GNOME Software, KDE Discover, etc). It should also work on virtually any 
distribution, and does not require root privileges.

1. Download the [latest Flatpak manifest](https://github.com/htkhiem/euphonica/releases/download/v0.12.0-alpha/io.github.htkhiem.Euphonica.json) to an empty folder somewhere.
2. Run `flatpak-builder` as follows:
  ```bash
  cd /path/to/flatpak/manifest
  flatpak-builder --force-clean --user --install-deps-from=flathub --repo=repo --install build-flatpak io.github.htkhiem.Euphonica.json
  ```
3. Once the above has completed, you can run Euphonica using:

  ``` bash
  flatpak run io.github.htkhiem.Euphonica
  ```

A desktop should also have been installed for you, although it might take a reboot to show up.

### Using Meson 

This builds Euphonica against system library packages, then installs it directly into `/usr/local/bin`.
It is the most lightweight option, but has only been tested on Arch Linux.

1. Make sure you have these dependencies installed beforehand:
  - `gtk4` >= 4.16
  - `libadwaita` >= 1.6
  - `rustup` >= 1.27
  - `meson` >= 1.5
  - `gettext` >= 0.23
  - `mpd` >= 0.21 (Euphonica relies on the new filter syntax)
  
    If you are on Arch Linux, `gettext` should have been installed as part of the `base-devel` metapackage, which also includes `git` (to clone this repo :) ).

2. Init build folder
  ```bash
  cd /path/to/where/to/clone/euphonica
  git clone https://github.com/htkhiem/euphonica.git
  cd euphonica
  git submodule update --init
  meson setup build --buildtype=release
  ```

3. Compile & install (will require root privileges)
  ```bash
  cd build
  meson install
  ```

## Setting up Euphonica with your MPD instance

Euphonica works just like any other MPD client in this regard. Passwords will be saved to your default (usually login) keyring, so in case you
have biometric login set up without also setting up TPM, you might need to manually unlock your login keyring before Euphonica can fetch its 
password back for reconnection. 

Optionally, your MPD instance should be configured with a playlist folder. If not configured, MPD will be unable to create and edit playlists.
Euphonica's playlist management features won't be available in this case.

It currently only supports connecting to MPD via a TCP socket. Local socket support might be added later when the planned features require it.

## Using Euphonica with your music library

Euphonica tries to make minimal and sensible assumptions about your library's folder structure. 

- Tracks from different releases (albums) should not be put in the same folder. Preferably, all tracks of the same release should be put in the same folder.
- In order to make your album art files available to Euphonica (and other MPD clients), name them `cover.png/jpg/webp` and put them in the same folder as the tracks themselves.
- (Optional) Use [Beets](https://beets.io/?trk=public_post-text) to tag your tracks, or follow its tag schema, for best results when fetching album arts and artist avatars.
Euphonica is developed with Beets tagging in mind and can take advantage of its MusicBrainz ID tags for accurate metadata fetching.

Most libraries, especially those that ran well with other MPD clients like [Cantata](https://github.com/CDrummond/cantata), should require no reorganisation.

## TODO
- Local storage management UI (to allow re-fetching metadata, clearing album art cache and the like)
- Support more stickers-based features:
  - Recently played
  - Per-song ratings
  - User-editable album wikis and artist bios
  - Metadata sync between Euphonica instances (instead of being stored locally)
  - Should follow existing sticker schemas, such as that proposed by myMPD, where possible.
- Special support for local socket connection, or remote filesystem access, to enable the following features:
  - Library management operations such as tag editing (will require access to the files themselves)
  - Save downloaded album arts and artist avatars directly into the music folders themselves so other instances
    and clients can use them.
- Browse by genre
- Realtime lyrics fetching
- An "All tracks" page with advanced, freeform querying to take full advantage of MPD v0.21+'s new query syntax
