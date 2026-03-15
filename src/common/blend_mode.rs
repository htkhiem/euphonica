use gtk::{gsk, glib};

/// Our version of gsk::BlendMode, with additional methods to facilitate storing in GSettings.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq, glib::Enum)]
#[enum_type(name = "EuphonicaBlendMode")]
pub enum BlendMode {
    #[default]
    Default = 0,
    Multiply = 1,
    Screen = 2,
    Overlay = 3,
    Darken = 4,
    Lighten = 5,
    Dodge = 6,
    Burn = 7,
    HardLight = 8,
    SoftLight = 9,
    Difference = 10,
    Exclusion = 11,
    Color = 12,
    Hue = 13,
    Saturation = 14,
    Luminosity = 15,
}

impl TryFrom<u32> for BlendMode {
    type Error = ();
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Default),
            1 => Ok(Self::Multiply),
            2 => Ok(Self::Screen),
            3 => Ok(Self::Overlay),
            4 => Ok(Self::Darken),
            5 => Ok(Self::Lighten),
            6 => Ok(Self::Dodge),
            7 => Ok(Self::Burn),
            8 => Ok(Self::HardLight),
            9 => Ok(Self::SoftLight),
            10 => Ok(Self::Difference),
            11 => Ok(Self::Exclusion),
            12 => Ok(Self::Color),
            13 => Ok(Self::Hue),
            14 => Ok(Self::Saturation),
            15 => Ok(Self::Luminosity),
            _ => Err(()),
        }
    }
}

impl From<BlendMode> for u32 {
    fn from(val: BlendMode) -> Self {
        match val {
            BlendMode::Default => 0,
            BlendMode::Multiply => 1,
            BlendMode::Screen => 2,
            BlendMode::Overlay => 3,
            BlendMode::Darken => 4,
            BlendMode::Lighten => 5,
            BlendMode::Dodge => 6,
            BlendMode::Burn => 7,
            BlendMode::HardLight => 8,
            BlendMode::SoftLight => 9,
            BlendMode::Difference => 10,
            BlendMode::Exclusion => 11,
            BlendMode::Color => 12,
            BlendMode::Hue => 13,
            BlendMode::Saturation => 14,
            BlendMode::Luminosity => 15,
        }
    }
}

impl From<gsk::BlendMode> for BlendMode {
    fn from(value: gsk::BlendMode) -> Self {
        match value {
            gsk::BlendMode::Default => Self::Default,
            gsk::BlendMode::Multiply => Self::Multiply,
            gsk::BlendMode::Screen => Self::Screen,
            gsk::BlendMode::Overlay => Self::Overlay,
            gsk::BlendMode::Darken => Self::Darken,
            gsk::BlendMode::Lighten => Self::Lighten,
            gsk::BlendMode::ColorDodge => Self::Dodge,
            gsk::BlendMode::ColorBurn => Self::Burn,
            gsk::BlendMode::HardLight => Self::HardLight,
            gsk::BlendMode::SoftLight => Self::SoftLight,
            gsk::BlendMode::Difference => Self::Difference,
            gsk::BlendMode::Exclusion => Self::Exclusion,
            gsk::BlendMode::Color => Self::Color,
            gsk::BlendMode::Saturation => Self::Saturation,
            gsk::BlendMode::Luminosity => Self::Luminosity,
            _ => unimplemented!(),
        }
    }
}

impl From<BlendMode> for gsk::BlendMode {
    fn from(val: BlendMode) -> Self {
        match val {
            BlendMode::Default => gsk::BlendMode::Default,
            BlendMode::Multiply => gsk::BlendMode::Multiply,
            BlendMode::Screen => gsk::BlendMode::Screen,
            BlendMode::Overlay => gsk::BlendMode::Overlay,
            BlendMode::Darken => gsk::BlendMode::Darken,
            BlendMode::Lighten => gsk::BlendMode::Lighten,
            BlendMode::Dodge => gsk::BlendMode::ColorDodge,
            BlendMode::Burn => gsk::BlendMode::ColorBurn,
            BlendMode::HardLight => gsk::BlendMode::HardLight,
            BlendMode::SoftLight => gsk::BlendMode::SoftLight,
            BlendMode::Difference => gsk::BlendMode::Difference,
            BlendMode::Exclusion => gsk::BlendMode::Exclusion,
            BlendMode::Color => gsk::BlendMode::Color,
            BlendMode::Hue => gsk::BlendMode::Hue,
            BlendMode::Saturation => gsk::BlendMode::Saturation,
            BlendMode::Luminosity => gsk::BlendMode::Luminosity,
        }
    }
}
