![Euphonica icon](data/icons/hicolor/scalable/apps/io.github.htkhiem.Euphonica.svg)
# Euphonica

[![Flathub Downloads](https://img.shields.io/flathub/downloads/io.github.htkhiem.Euphonica?style=flat-square&logo=flathub)](https://flathub.org/en/apps/io.github.htkhiem.Euphonica)

An MPD frontend with delusions of grandeur. 

It exists to sate my need for something that's got the bling and the features to back that bling up.

## Features
- Adaptive GTK4+`libadwaita` UI for most MPD features, from queue reordering and ReplayGain to crossfade and MixRamp configuration.
- Practically **zero-cost** static background blur powered by [libblur](https://github.com/awxkee/libblur). Go ham with blur radius!
- Customisable spectrum visualiser, reading from MPD FIFO or system PipeWire.
- Automatic accent colours based on album art (optional).
- Advanced client-side dynamic playlists.
  - Both query-based and sticker-based filtering rules are supported at the same time.
  - Multiple ordering clauses (or random shuffle on refresh).
  - Auto-refresh scheduling (hourly, daily, weekly, etc).
  - Optional fetch limit for things like top-10 playlists.
  - Graphical rules editor with live error checking.
  - Save a dynamic playlist's current state as an MPD-side static playlist whenver you want.
  - JSON import/export for sharing & backing up dynamic playlist rules.
- Fetch album arts, artist avatars and synced song lyrics from external sources (currently supports Last.fm, MusicBrainz and LRCLIB).
- myMPD-compatible stickers handling.
- Integrated MPRIS client with background run supported. The background instance can be reopened via your shell's MPRIS applet, the "Background applications" section in GNOME's quick settings shade (if installed via Flatpak) or simply by launching Euphonica again.
- Rate albums (requires MPD 0.24+) and individual songs.
- Audio quality indicators (lossy, lossless, hi-res, DSD) for individual songs as well as albums & detailed format printout.
- Asynchronous search for large collections. The app as a whole should work with any library size (tested with up to 30K songs).
- Configurable multi-artist tag syntax, works with anything you throw at it.
  - In other words, your artist tags can be pretty messy and Euphonica will still be able to correctly split them into individual artists.
- Performant album art fetching & display (LRU-cached to both cut down on disk reads and RAM usage).
- Volume knob with dBFS readout support ('cuz why not?).
- User-friendly configuration UI & GSettings backend.
- MPD passwords are securely stored in your user's login keyring.
- Commands are bundled into lists for efficient MPD-side processing where possible.

## Screenshots

The below were captured with a mix of dark and light modes.


- Album View[^1]
  <img width="2100" height="1500" alt="album-view" src="https://github.com/user-attachments/assets/a262c79b-6078-4075-9986-7311fefc4d8e" />
  
- UI at different sizes[^1]
  <img width="2100" height="1500" alt="mini-ui" src="https://github.com/user-attachments/assets/234e4fb8-2d3c-444c-b2cc-605147989f35" />

- Queue View[^1]
  <img width="2100" height="1500" alt="queue-view" src="https://github.com/user-attachments/assets/585547f5-95ca-4de7-b8c5-bd3de14948cd" />

- Album wiki as fetched from Last.fm[^1][^2]
  <img width="2100" height="1500" alt="album-content-view" src="https://github.com/user-attachments/assets/5ef8bcbc-53f2-44df-bb0d-8711bf03429f" />
  
- Dynamic Playlist Editor[^1]
  <img width="2100" height="1500" alt="dyn-playlist-editor" src="https://github.com/user-attachments/assets/971b1c88-59d1-4750-a72b-6af56bc7aa9a" />

- Settings GUI for pretty much everything[^1]
  <img width="2100" height="1500" alt="viz-settings" src="https://github.com/user-attachments/assets/efc07616-e74a-4955-8f8b-da4c5762537d" />


[^1]: Actual album arts and artist images have been replaced with random pictures from [Pexels](https://www.pexels.com/). All credits go to the original authors.
[^2]: Artist bios and album wikis are user-contributed and licensed by Last.fm under CC-BY-SA.

## Installation

Euphonica is still in very early development, and so far has only been tested on Arch Linux (btw).

The preferred way to install Euphonica is as a Flatpak app via Flathub:

<a href='https://flathub.org/apps/io.github.htkhiem.Euphonica'>
    <img width='240' alt='Get it on Flathub' src='https://flathub.org/api/badge?locale=en'/>
</a>

Other ways to install Euphonica are listed below:

<details>
  <summary><h3>Arch Linux</h3></summary>
  
  An (admittedly experimental) AUR package is [now available](https://aur.archlinux.org/packages/euphonica-git).

  ```bash
  # Use your favourite AUR helper here
  paru -S euphonica-git
  ```
  
</details>

<details>
  <summary><h3>Nixpkgs</h3></summary>

  The Nix package is kindly maintained by [@paperdigits](https://github.com/paperdigits) [here](https://search.nixos.org/packages?channel=unstable&show=euphonica&from=0&size=50&sort=relevance&type=packages).
</details>

## Set-up

Euphonica requires some preparation before it can be used, especially if you have never used an MPD client before. Please see the [wiki article](https://github.com/htkhiem/euphonica/wiki/Installation-&-basic-local-configuration) for instructions on setting up a basic local instance.

## Build

<details>
  <summary><h3>Flatpak</h3></summary>

  Euphonica can also be built from source using `flatpak-builder`.
  
  This builds and installs Euphonica as a sandboxed Flatpak app on your system, complete with an entry in 
  Flatpak-aware app stores (like GNOME Software, KDE Discover, etc). It should also work on virtually any 
  distribution, and does not require root privileges. Unlike installing from Flathub, this always builds
  the **latest** commit and as such is more suitable for development and testing purposes. Also, it might
  **overwrite** the existing Flathub-distributed installation in case you have one.
  
  1. Add the Flathub repo in case you haven't already:
     
  ```bash
  flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
  ```
  2. Run `flatpak-builder` as follows:
     
  ```bash
  cd /path/to/where/you/cloned/euphonica
  flatpak-builder --force-clean --user --install-deps-from=flathub --repo=repo --install build-flatpak io.github.htkhiem.Euphonica-dev.json
  ```
  3. Once the above has completed, you can run Euphonica using:
  
  ```bash
  flatpak run io.github.htkhiem.Euphonica
  ```
  
  
  A desktop icon entry should also have been installed for you, although it might take a reboot to show up.
  
</details>

<details>
  <summary><h3>Meson</h3></summary>

  This builds Euphonica against system library packages, then installs it directly into `/usr/local/bin`.
  It is the most lightweight option, but has only been tested on Arch Linux.
  
  1. Make sure you have these dependencies installed beforehand:
    - `gtk4` >= 4.18
    - `libadwaita` >= 1.7
    - `meson` >= 1.5
    - `gettext` >= 0.23
    - `mpd` >= 0.24 (Euphonica relies on the new filter syntax and expanded tagging)
    - `sqlite` (metadata store dependency)
    - An `xdg-desktop-portal` provider
    - The latest stable Rust toolchain. I highly recommend using `rustup` to manage them. Using it, you can install the latest stable toolchain using `rustup default stable` or update your existing one with `rustup update`. Ensure that `rustc` and `cargo` are of at least version `1.88.0`.
    
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
</details>

## TODO
- For v1.0, think of a new UI to cleanly resolve the following issues:
  - Recent View not being very useful.
  - **More browser-like navigation between pages, with actual navigation history instead of leading back to the outermost view**.
  - Album x Genre grouping.
  - A "Tracks" view that displays the library track by track, with advanced sorting & filtering.
  - Advanced filtering and sorting in Albums, Artists and Tracks View.
  - Make use efficient use of screen real estate on larger displays (currently looks nice for 1080p & below, but kind of stretched above that).
  - Clutter & inconveniences with having both a bottom player bar and a sidebar.
  - Less cluttered & more compact bottom bar, should we keep it.
    - UI should look symmetrical too as the bottom bar already is.
  - A less out-of-place and more touch-friendly volume knob design.
- User-editable album wikis and artist bios
- Metadata sync between Euphonica instances (instead of being stored locally)
- Local socket-exclusive features:
  - Library management operations such as tag editing (will require access to the files themselves)
  - Save downloaded album arts and artist avatars directly into the music folders themselves so other instances
    and clients can use them.
