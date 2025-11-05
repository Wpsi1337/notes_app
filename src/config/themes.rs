use std::collections::HashSet;

use super::ThemeName;

#[derive(Debug, Clone)]
pub struct ThemeRegistry {
    names: HashSet<ThemeName>,
}

impl ThemeRegistry {
    pub fn contains(&self, theme: &ThemeName) -> bool {
        self.names.contains(theme)
    }

    pub fn all(&self) -> impl Iterator<Item = &ThemeName> {
        self.names.iter()
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        let names = [
            ThemeName::Dark,
            ThemeName::Light,
            ThemeName::HighContrast,
            ThemeName::Solarized,
        ]
        .into_iter()
        .collect();
        Self { names }
    }
}
