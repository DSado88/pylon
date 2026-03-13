/// Terminal emulator configuration with sensible defaults.
pub struct CockpitConfig {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub sidebar_width: u32,
    pub sidebar_visible: bool,
    pub colors: ColorScheme,
}

pub struct ColorScheme {
    pub foreground: [f32; 4],
    pub background: [f32; 4],
    pub cursor: [f32; 4],
    pub ansi: [[f32; 4]; 16],
}

impl Default for CockpitConfig {
    fn default() -> Self {
        Self {
            font_family: String::from("JetBrains Mono"),
            font_size: 14.0,
            scrollback_lines: 10_000,
            sidebar_width: 280,
            sidebar_visible: true,
            colors: ColorScheme::default(),
        }
    }
}

impl Default for ColorScheme {
    fn default() -> Self {
        // Solarized Dark
        Self {
            foreground: [0.514, 0.580, 0.588, 1.0], // base0  #839496
            background: [0.000, 0.169, 0.212, 1.0], // base03 #002b36
            cursor: [0.396, 0.482, 0.514, 1.0],     // base01 #657b83
            ansi: [
                [0.027, 0.212, 0.259, 1.0], // black   (base02) #073642
                [0.863, 0.196, 0.184, 1.0], // red              #dc322f
                [0.522, 0.600, 0.000, 1.0], // green            #859900
                [0.710, 0.537, 0.000, 1.0], // yellow           #b58900
                [0.149, 0.545, 0.824, 1.0], // blue             #268bd2
                [0.827, 0.212, 0.510, 1.0], // magenta          #d33682
                [0.165, 0.631, 0.596, 1.0], // cyan             #2aa198
                [0.933, 0.910, 0.835, 1.0], // white   (base2)  #eee8d5
                [0.000, 0.169, 0.212, 1.0], // bright black  (base03) #002b36
                [0.796, 0.294, 0.086, 1.0], // bright red    (orange) #cb4b16
                [0.345, 0.431, 0.459, 1.0], // bright green  (base01) #586e75
                [0.396, 0.482, 0.514, 1.0], // bright yellow (base00) #657b83
                [0.514, 0.580, 0.588, 1.0], // bright blue   (base0)  #839496
                [0.424, 0.443, 0.769, 1.0], // bright magenta(violet) #6c71c4
                [0.576, 0.631, 0.631, 1.0], // bright cyan   (base1)  #93a1a1
                [0.992, 0.965, 0.890, 1.0], // bright white  (base3)  #fdf6e3
            ],
        }
    }
}
