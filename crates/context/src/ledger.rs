//! The set of blocks a log describes, folded from `context/compressed` events.
//!
//! The ledger is a *view*, never a store: it is rebuilt from events on every
//! open, exactly like every other projection in this project. That is why a
//! compressed range still exists after a restart, after a fork and after the
//! process is killed mid-turn — and why deleting the ledger costs nothing.

use crate::block::{Block, ERR_BAD_BLOCK, ERR_OVERLAP};
use crate::wire;
use hearth_core::event::Event;
use hearth_core::{Error, ErrorKind, Result};

/// One match from [`Ledger::search`]. The block is cloned because the caller
/// usually outlives the borrow (it is formatting a tool result), and a block is
/// a few hundred bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub block: Block,
    /// How many times the query terms appear in the summary. Only used to sort
    /// hits; a caller must not treat it as a confidence score.
    pub score: usize,
}

/// Every block in one log, ordered by the range each one covers.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    blocks: Vec<Block>,
}

impl Ledger {
    pub fn new() -> Ledger {
        Ledger { blocks: Vec::new() }
    }

    /// Folds the blocks out of a log.
    ///
    /// A malformed `context/compressed` record is an error rather than a skip:
    /// the type is known, so a bad payload means the log is corrupt, and a
    /// silently dropped block would make the history quietly claim that a range
    /// it hid is visible again.
    pub fn from_events(events: &[Event]) -> Result<Ledger> {
        let mut ledger = Ledger::new();
        for event in events {
            if let Some(block) = wire::parse_block(event)? {
                ledger.insert(block)?;
            }
        }
        Ok(ledger)
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn get(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|block| block.id == id)
    }

    /// The block that hides `seq`, if any. At most one can, because overlap is
    /// refused; a second answer would make "restore" ambiguous.
    pub fn covering(&self, seq: u64) -> Option<&Block> {
        self.blocks.iter().find(|block| block.covers(seq))
    }

    pub fn hides(&self, seq: u64) -> bool {
        self.covering(seq).is_some()
    }

    /// Total number of seq values hidden across all blocks.
    pub fn hidden_span(&self) -> u64 {
        self.blocks.iter().map(Block::span).sum()
    }

    /// Adds a block, refusing the two states that cannot be addressed later.
    pub fn insert(&mut self, block: Block) -> Result<()> {
        block.validate()?;
        if self.get(&block.id).is_some() {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("{ERR_BAD_BLOCK}: block {} already exists", block.id),
            ));
        }
        if let Some(other) = self
            .blocks
            .iter()
            .find(|other| other.overlaps(block.from, block.to))
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "{ERR_OVERLAP}: [{}..{}] shares seq with block {} covering [{}..{}]",
                    block.from, block.to, other.id, other.from, other.to
                ),
            ));
        }
        // Keep the order stable by range so search output does not depend on
        // the order events happened to arrive in.
        let at = self
            .blocks
            .iter()
            .position(|other| other.from > block.from)
            .unwrap_or(self.blocks.len());
        self.blocks.insert(at, block);
        Ok(())
    }

    /// Finds blocks whose summary mentions every term, case-insensitively.
    ///
    /// This searches the summaries, not the originals: the point of a block is
    /// that the originals are not in the window any more. To read them, restore
    /// the block.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .filter(|term| !term.is_empty())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .blocks
            .iter()
            .filter_map(|block| {
                let haystack = block.summary.to_lowercase();
                let mut score = 0usize;
                for term in &terms {
                    let found = haystack.matches(term.as_str()).count();
                    if found == 0 {
                        return None;
                    }
                    score += found;
                }
                Some(Hit {
                    block: block.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.block.from.cmp(&right.block.from))
        });
        hits
    }
}
