//! Container image, runtime, and storage values used by the V2 deploy path.

use std::collections::BTreeMap;
use std::num::{NonZeroI64, NonZeroU16, NonZeroU64};

use serde::{Deserialize, Serialize};

use crate::ids::NamespaceId;

pub mod images;
pub mod runtime;
pub mod volume;

pub use images::*;
pub use runtime::*;
pub use volume::*;
