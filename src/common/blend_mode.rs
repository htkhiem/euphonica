use gtk::gsk;

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

impl Into<u32> for BlendMode {
    fn into(self) -> u32 {
        match self {
            Self::Default => 0,
            Self::Multiply => 1,
            Self::Screen => 2,
            Self::Overlay => 3,
            Self::Darken => 4,
            Self::Lighten => 5,
            Self::Dodge => 6,
            Self::Burn => 7,
            Self::HardLight => 8,
            Self::SoftLight => 9,
            Self::Difference => 10,
            Self::Exclusion => 11,
            Self::Color => 12,
            Self::Hue => 13,
            Self::Saturation => 14,
            Self::Luminosity => 15,
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

impl Into<gsk::BlendMode> for BlendMode {
    fn into(self) -> gsk::BlendMode {
        match self {
            Self::Default => gsk::BlendMode::Default,
            Self::Multiply => gsk::BlendMode::Multiply,
            Self::Screen => gsk::BlendMode::Screen,
            Self::Overlay => gsk::BlendMode::Overlay,
            Self::Darken => gsk::BlendMode::Darken,
            Self::Lighten => gsk::BlendMode::Lighten,
            Self::Dodge => gsk::BlendMode::ColorDodge,
            Self::Burn => gsk::BlendMode::ColorBurn,
            Self::HardLight => gsk::BlendMode::HardLight,
            Self::SoftLight => gsk::BlendMode::SoftLight,
            Self::Difference => gsk::BlendMode::Difference,
            Self::Exclusion => gsk::BlendMode::Exclusion,
            Self::Color => gsk::BlendMode::Color,
            Self::Hue => gsk::BlendMode::Hue,
            Self::Saturation => gsk::BlendMode::Saturation,
            Self::Luminosity => gsk::BlendMode::Luminosity,
        }
    }
}
