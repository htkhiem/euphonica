use gtk::gdk::Texture;
use once_cell::sync::Lazy;

pub static ALBUMART_PLACEHOLDER: Lazy<Texture> = Lazy::new(|| {
    println!("Loading placeholder texture...");
    Texture::from_resource("/io/github/htkhiem/Euphonica/albumart-placeholder.svg")
});

pub static ALBUMART_THUMBNAIL_PLACEHOLDER: Lazy<Texture> = Lazy::new(|| {
    println!("Loading placeholder texture...");
    Texture::from_resource("/io/github/htkhiem/Euphonica/albumart-placeholder-thumb.png")
});

pub static EMPTY_ALBUM_STRING: Lazy<&str> = Lazy::new(|| {"(untitled album)"});
pub static EMPTY_ARTIST_STRING: Lazy<&str> = Lazy::new(|| {"(unknown artist)"});
