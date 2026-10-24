//! A tiny counter contract used to demo soroban-cost-snapshot.
#![no_std]
use soroban_sdk::{contract, contractevent, contractimpl, symbol_short, Env, Symbol};

const KEY: Symbol = symbol_short!("count");

/// Emitted when the counter changes.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Incremented {
    pub value: u32,
}

#[contract]
pub struct Counter;

#[contractimpl]
impl Counter {
    /// Adds `by` to the counter and returns the new value.
    pub fn incr(env: Env, by: u32) -> u32 {
        let mut v: u32 = env.storage().persistent().get(&KEY).unwrap_or(0);
        v = v.saturating_add(by);
        #[cfg(feature = "slow")]
        {
            // "Accidental" extra work, e.g. re-reading storage in a loop.
            for _ in 0..20 {
                let _: Option<u32> = env.storage().persistent().get(&KEY);
            }
        }
        env.storage().persistent().set(&KEY, &v);
        Incremented { value: v }.publish(&env);
        v
    }

    /// Returns the current value.
    pub fn get(env: Env) -> u32 {
        env.storage().persistent().get(&KEY).unwrap_or(0)
    }
}

#[cfg(test)]
mod test {
    extern crate std;
    use super::*;
    use soroban_cost_snapshot::cost_snapshot;

    #[test]
    fn costs() {
        let env = Env::default();
        let id = env.register(Counter, ());
        let c = CounterClient::new(&env, &id);

        assert_eq!(c.incr(&5), 5);
        cost_snapshot!(env, "counter::incr");

        assert_eq!(c.get(), 5);
        cost_snapshot!(env, "counter::get");
    }
}
