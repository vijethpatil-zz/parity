//! Ethereum virtual machine.

pub mod ext;
pub mod evm;
/// TODO [Tomusdrw] Please document me
pub mod interpreter;
#[macro_use]
pub mod factory;
pub mod schedule;
mod instructions;
#[cfg(feature = "jit" )]
mod jit;

#[cfg(test)]
mod tests;

pub use self::evm::{Evm, Error, Result};
pub use self::ext::{Ext, ContractCreateResult, MessageCallResult};
pub use self::factory::Factory;
pub use self::schedule::Schedule;
pub use self::factory::VMType;