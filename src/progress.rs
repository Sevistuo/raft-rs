// Copyright 2016 PingCAP, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

// Copyright 2015 The etcd Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use errors::Error;
use fxhash::FxHashMap;
use std::cmp;
use std::collections::hash_map::HashMap;

/// The state of the progress.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ProgressState {
    /// Whether it's probing.
    Probe,
    /// Whether it's replicating.
    Replicate,
    /// Whethers it's a snapshot.
    Snapshot,
}

impl Default for ProgressState {
    fn default() -> ProgressState {
        ProgressState::Probe
    }
}

/// `ProgressSet` contains several `Progress`es,
/// which could be `Leader`, `Follower` and `Learner`.
#[derive(Default, Clone)]
pub struct ProgressSet {
    voters: FxHashMap<u64, Progress>,
    learners: FxHashMap<u64, Progress>,
}

impl ProgressSet {
    /// Creates a new ProgressSet.
    pub fn new(voter_size: usize, learner_size: usize) -> Self {
        ProgressSet {
            voters: HashMap::with_capacity_and_hasher(voter_size, Default::default()),
            learners: HashMap::with_capacity_and_hasher(learner_size, Default::default()),
        }
    }

    /// Returns the status of voters.
    pub fn voters(&self) -> &FxHashMap<u64, Progress> {
        &self.voters
    }

    /// Returns the status of learners.
    pub fn learners(&self) -> &FxHashMap<u64, Progress> {
        &self.learners
    }

    /// Returns the ids of all known nodes.
    pub fn nodes(&self) -> Vec<u64> {
        let mut nodes = Vec::with_capacity(self.voters.len());
        nodes.extend(self.voters.keys());
        nodes.sort();
        nodes
    }

    /// Returns the ids of all known learners.
    pub fn learner_nodes(&self) -> Vec<u64> {
        let mut ids = Vec::with_capacity(self.learners.len());
        ids.extend(self.learners.keys());
        ids.sort();
        ids
    }

    /// Grabs a reference to the progress of a node.
    pub fn get(&self, id: u64) -> Option<&Progress> {
        self.voters.get(&id).or_else(|| self.learners.get(&id))
    }

    /// Grabs a mutable reference to the progress of a node.
    pub fn get_mut(&mut self, id: u64) -> Option<&mut Progress> {
        let progress = self.voters.get_mut(&id);
        if progress.is_none() {
            return self.learners.get_mut(&id);
        }
        progress
    }

    /// Returns an iterator across all the nodes and their progress.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &Progress)> {
        self.voters.iter().chain(&self.learners)
    }

    /// Returns a mutable iterator across all the nodes and their progress.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&u64, &mut Progress)> {
        self.voters.iter_mut().chain(&mut self.learners)
    }

    /// Adds a voter node
    pub fn insert_voter(&mut self, id: u64, pr: Progress) -> Result<(), Error> {
        if self.voters.contains_key(&id) {
            Err(Error::Exists(id, "voters"))?
        }
        if self.learners.contains_key(&id) {
            Err(Error::Exists(id, "learners"))?;
        }
        self.voters.insert(id, pr);
        Ok(())
    }

    /// Adds a learner to the cluster
    pub fn insert_learner(&mut self, id: u64, pr: Progress) -> Result<(), Error> {
        if self.voters.contains_key(&id) {
            Err(Error::Exists(id, "voters"))?
        }
        if self.learners.contains_key(&id) {
            Err(Error::Exists(id, "learners"))?
        }
        self.learners.insert(id, pr);
        Ok(())
    }

    /// Removes the peer from the set of voters or learners.
    pub fn remove(&mut self, id: u64) -> Option<Progress> {
        match self.voters.remove(&id) {
            None => self.learners.remove(&id),
            some => some,
        }
    }

    /// Promote a learner to a peer.
    pub fn promote_learner(&mut self, id: u64) -> Result<(), Error> {
        if self.voters.contains_key(&id) {
            Err(Error::Exists(id, "voters"))?;
        }
        // We don't want to remove it unless it's there.
        if self.learners.contains_key(&id) {
            let mut learner = self.learners.remove(&id).unwrap(); // We just checked!
            learner.is_learner = false;
            self.voters.insert(id, learner);
            Ok(())
        } else {
            Err(Error::NotExists(id, "learners"))
        }
    }
}

/// The progress of catching up from a restart.
#[derive(Debug, Default, Clone)]
pub struct Progress {
    /// How much state is matched.
    pub matched: u64,
    /// The next index to apply
    pub next_idx: u64,
    /// When in ProgressStateProbe, leader sends at most one replication message
    /// per heartbeat interval. It also probes actual progress of the follower.
    ///
    /// When in ProgressStateReplicate, leader optimistically increases next
    /// to the latest entry sent after sending replication message. This is
    /// an optimized state for fast replicating log entries to the follower.
    ///
    /// When in ProgressStateSnapshot, leader should have sent out snapshot
    /// before and stop sending any replication message.
    pub state: ProgressState,
    /// Paused is used in ProgressStateProbe.
    /// When Paused is true, raft should pause sending replication message to this peer.
    pub paused: bool,
    /// This field is used in ProgressStateSnapshot.
    /// If there is a pending snapshot, the pendingSnapshot will be set to the
    /// index of the snapshot. If pendingSnapshot is set, the replication process of
    /// this Progress will be paused. raft will not resend snapshot until the pending one
    /// is reported to be failed.
    pub pending_snapshot: u64,

    /// This is true if the progress is recently active. Receiving any messages
    /// from the corresponding follower indicates the progress is active.
    /// RecentActive can be reset to false after an election timeout.
    pub recent_active: bool,

    /// Inflights is a sliding window for the inflight messages.
    /// When inflights is full, no more message should be sent.
    /// When a leader sends out a message, the index of the last
    /// entry should be added to inflights. The index MUST be added
    /// into inflights in order.
    /// When a leader receives a reply, the previous inflights should
    /// be freed by calling inflights.freeTo.
    pub ins: Inflights,

    /// Indicates the Progress is a learner or not.
    pub is_learner: bool,
}

impl Progress {
    fn reset_state(&mut self, state: ProgressState) {
        self.paused = false;
        self.pending_snapshot = 0;
        self.state = state;
        self.ins.reset();
    }

    /// Changes the progress to a probe.
    pub fn become_probe(&mut self) {
        // If the original state is ProgressStateSnapshot, progress knows that
        // the pending snapshot has been sent to this peer successfully, then
        // probes from pendingSnapshot + 1.
        if self.state == ProgressState::Snapshot {
            let pending_snapshot = self.pending_snapshot;
            self.reset_state(ProgressState::Probe);
            self.next_idx = cmp::max(self.matched + 1, pending_snapshot + 1);
        } else {
            self.reset_state(ProgressState::Probe);
            self.next_idx = self.matched + 1;
        }
    }

    /// Changes the progress to a Replicate.
    pub fn become_replicate(&mut self) {
        self.reset_state(ProgressState::Replicate);
        self.next_idx = self.matched + 1;
    }

    /// Changes the progress to a snapshot.
    pub fn become_snapshot(&mut self, snapshot_idx: u64) {
        self.reset_state(ProgressState::Snapshot);
        self.pending_snapshot = snapshot_idx;
    }

    /// Sets the snapshot to failure.
    pub fn snapshot_failure(&mut self) {
        self.pending_snapshot = 0;
    }

    /// Unsets pendingSnapshot if Match is equal or higher than
    /// the pendingSnapshot
    pub fn maybe_snapshot_abort(&self) -> bool {
        self.state == ProgressState::Snapshot && self.matched >= self.pending_snapshot
    }

    /// Returns false if the given n index comes from an outdated message.
    /// Otherwise it updates the progress and returns true.
    pub fn maybe_update(&mut self, n: u64) -> bool {
        let need_update = self.matched < n;
        if need_update {
            self.matched = n;
            self.resume();
        };

        if self.next_idx < n + 1 {
            self.next_idx = n + 1
        }

        need_update
    }

    /// Optimistically advance the index
    pub fn optimistic_update(&mut self, n: u64) {
        self.next_idx = n + 1;
    }

    /// Returns false if the given index comes from an out of order message.
    /// Otherwise it decreases the progress next index to min(rejected, last)
    /// and returns true.
    pub fn maybe_decr_to(&mut self, rejected: u64, last: u64) -> bool {
        if self.state == ProgressState::Replicate {
            // the rejection must be stale if the progress has matched and "rejected"
            // is smaller than "match".
            if rejected <= self.matched {
                return false;
            }
            self.next_idx = self.matched + 1;
            return true;
        }

        // the rejection must be stale if "rejected" does not match next - 1
        if self.next_idx == 0 || self.next_idx - 1 != rejected {
            return false;
        }

        self.next_idx = cmp::min(rejected, last + 1);
        if self.next_idx < 1 {
            self.next_idx = 1;
        }
        self.resume();
        true
    }

    /// Determine whether progress is paused.
    pub fn is_paused(&self) -> bool {
        match self.state {
            ProgressState::Probe => self.paused,
            ProgressState::Replicate => self.ins.full(),
            ProgressState::Snapshot => true,
        }
    }

    /// Resume progress
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Pause progress.
    pub fn pause(&mut self) {
        self.paused = true;
    }
}

/// A buffer of inflight messages.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Inflights {
    // the starting index in the buffer
    start: usize,
    // number of inflights in the buffer
    count: usize,

    // ring buffer
    buffer: Vec<u64>,
}

impl Inflights {
    /// Creates a new buffer for inflight messages.
    pub fn new(cap: usize) -> Inflights {
        Inflights {
            buffer: Vec::with_capacity(cap),
            ..Default::default()
        }
    }

    /// Returns true if the inflights is full.
    pub fn full(&self) -> bool {
        self.count == self.cap()
    }

    /// The buffer capacity.
    pub fn cap(&self) -> usize {
        self.buffer.capacity()
    }

    /// Adds an inflight into inflights
    pub fn add(&mut self, inflight: u64) {
        if self.full() {
            panic!("cannot add into a full inflights")
        }

        let mut next = self.start + self.count;
        if next >= self.cap() {
            next -= self.cap();
        }
        assert!(next <= self.buffer.len());
        if next == self.buffer.len() {
            self.buffer.push(inflight);
        } else {
            self.buffer[next] = inflight;
        }
        self.count += 1;
    }

    /// Frees the inflights smaller or equal to the given `to` flight.
    pub fn free_to(&mut self, to: u64) {
        if self.count == 0 || to < self.buffer[self.start] {
            // out of the left side of the window
            return;
        }

        let mut i = 0usize;
        let mut idx = self.start;
        while i < self.count {
            if to < self.buffer[idx] {
                // found the first large inflight
                break;
            }

            // increase index and maybe rotate
            idx += 1;
            if idx >= self.cap() {
                idx -= self.cap();
            }

            i += 1;
        }

        // free i inflights and set new start index
        self.count -= i;
        self.start = idx;
    }

    /// Frees the first buffer entry.
    pub fn free_first_one(&mut self) {
        let start = self.buffer[self.start];
        self.free_to(start);
    }

    /// Frees all inflights.
    pub fn reset(&mut self) {
        self.count = 0;
        self.start = 0;
    }
}

#[cfg(test)]
mod test {
    use progress::Inflights;
    use setup_for_test;

    #[test]
    fn test_inflight_add() {
        setup_for_test();
        let mut inflight = Inflights::new(10);
        for i in 0..5 {
            inflight.add(i);
        }

        let wantin = Inflights {
            start: 0,
            count: 5,
            buffer: vec![0, 1, 2, 3, 4],
        };

        assert_eq!(inflight, wantin);

        for i in 5..10 {
            inflight.add(i);
        }

        let wantin2 = Inflights {
            start: 0,
            count: 10,
            buffer: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin2);

        let mut inflight2 = Inflights {
            start: 5,
            buffer: Vec::with_capacity(10),
            ..Default::default()
        };
        inflight2.buffer.extend_from_slice(&vec![0, 0, 0, 0, 0]);

        for i in 0..5 {
            inflight2.add(i);
        }

        let wantin21 = Inflights {
            start: 5,
            count: 5,
            buffer: vec![0, 0, 0, 0, 0, 0, 1, 2, 3, 4],
        };

        assert_eq!(inflight2, wantin21);

        for i in 5..10 {
            inflight2.add(i);
        }

        let wantin22 = Inflights {
            start: 5,
            count: 10,
            buffer: vec![5, 6, 7, 8, 9, 0, 1, 2, 3, 4],
        };

        assert_eq!(inflight2, wantin22);
    }

    #[test]
    fn test_inflight_free_to() {
        setup_for_test();
        let mut inflight = Inflights::new(10);
        for i in 0..10 {
            inflight.add(i);
        }

        inflight.free_to(4);

        let wantin = Inflights {
            start: 5,
            count: 5,
            buffer: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin);

        inflight.free_to(8);

        let wantin2 = Inflights {
            start: 9,
            count: 1,
            buffer: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin2);

        for i in 10..15 {
            inflight.add(i);
        }

        inflight.free_to(12);

        let wantin3 = Inflights {
            start: 3,
            count: 2,
            buffer: vec![10, 11, 12, 13, 14, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin3);

        inflight.free_to(14);

        let wantin4 = Inflights {
            start: 5,
            count: 0,
            buffer: vec![10, 11, 12, 13, 14, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin4);
    }

    #[test]
    fn test_inflight_free_first_one() {
        setup_for_test();
        let mut inflight = Inflights::new(10);
        for i in 0..10 {
            inflight.add(i);
        }

        inflight.free_first_one();

        let wantin = Inflights {
            start: 1,
            count: 9,
            buffer: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        };

        assert_eq!(inflight, wantin);
    }
}
