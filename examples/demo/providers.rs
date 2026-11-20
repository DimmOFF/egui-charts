//! The data providers the app settings leave on, shared by the feeds, the quotes and the symbol
//! search so turning one off stops using its market data.

use std::sync::{Arc, RwLock};

use egui_charts::services::DataProviders;

/// The providers left on, read by every venue on each request.
#[derive(Clone, Default)]
pub struct Providers(Arc<RwLock<DataProviders>>);

impl Providers {
    /// The providers the settings hold now.
    pub fn get(&self) -> DataProviders {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Takes the providers the settings hold now.
    pub fn set(&self, providers: &DataProviders) {
        *self.0.write().unwrap_or_else(|error| error.into_inner()) = providers.clone();
    }
}
