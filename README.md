![Euphonica icon](data/icons/hicolor/scalable/apps/io.github.htkhiem.Euphonica.svg)
# Euphonica

An MPD frontend with delusions of grandeur. 

## Features
- GTK4 Libadwaita UI for most MPD features, from basic things like playback controls, queue reordering and ReplayGain to things like output control, crossfade and MixRamp configuration
- Built-in, customisable spectrum visualiser, reading from MPD FIFO or system PipeWire.
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

- Album View in light mode[^1]
  ![album-view-light](https://github.com/user-attachments/assets/302b0e37-ff4a-4151-b688-6d6b5e1dcb60)

- Queue View in dark mode[^1]
  ![queue-view-dark](https://github.com/user-attachments/assets/0ed85f77-48dc-4dc6-9e43-850e731bcaaa)

- Artist bio as fetched from Last.fm[^1][^2][^3]
  ![artist-content-view-dark](https://github.com/user-attachments/assets/659c0ca3-08f6-44f5-9840-26060057fad7)

- Album wiki as fetched from Last.fm[^1][^2]
  ![album-content-view-dark](https://github.com/user-attachments/assets/2665a48a-e81c-43ff-b3c1-76a6d7443f5e)
  
- Playlist Content View in dark mode[^1]
  ![playlist-content-view-dark](https://github.com/user-attachments/assets/42c3b9c5-a4fe-4e70-9765-5326c75d6c5b)

  
- Some of the available UI customisations[^1]
![visualiser-customisation](https://github.com/user-attachments/assets/e21d0a36-6dba-4fea-84a2-0fdb0810bff5)

[^1]: Actual album arts have been replaced with random pictures from [Pexels](https://www.pexels.com/). All credits go to the original authors.
[^2]: Artist bios and album wikis are user-contributed and licensed by Last.fm under CC-BY-SA.
[^3]: The displayed image has been released into the public domain. More information at [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Johann_Sebastian_Bach.jpg).

## Build

Euphonica is still in very early development, and so far has only been tested on Arch Linux (btw).

1. Make sure you have these dependencies installed beforehand:
  - `gtk4` >= 4.16
  - `libadwaita` >= 1.5
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

Flatpak & AUR releases are also planned.

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
- Stickers DB support to enable the following features:
  - Recently played
  - Ratings (song, album, etc)
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
