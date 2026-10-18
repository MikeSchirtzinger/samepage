//! State roles, transitions and diagnostic sentences.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StateMachineView {
    pub id: String,
    pub title: String,
    pub states: Vec<StateView>,
    pub transitions: Vec<TransitionView>,
    pub current_state: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StateView {
    pub id: String,
    pub label: String,
    pub initial: bool,
    pub terminal: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TransitionView {
    pub id: String,
    pub from: String,
    pub from_label: String,
    pub to: String,
    pub to_label: String,
    pub to_machine: String,
    pub to_machine_label: String,
    pub event: String,
    pub guard: String,
    pub order: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Problem {
    pub code: &'static str,
    pub ids: Vec<String>,
    pub detail: String,
}

impl StateMachineView {
    pub fn describe(&self) -> String {
        let cyclic = self.is_cyclic();
        let cycle_note = if cyclic { ", cyclic" } else { "" };
        let mut out = format!(
            "\nSTATE MACHINE {:?} ({} states, {} transitions{cycle_note})\n",
            self.title,
            self.states.len(),
            self.transitions.len()
        );
        if let Some(current_state) = &self.current_state {
            out.push_str(current_state);
            out.push('\n');
        }
        for state in &self.states {
            let mut roles = Vec::new();
            if state.initial {
                roles.push("INITIAL");
            }
            if state.terminal {
                roles.push("TERMINAL");
            }
            let suffix = if roles.is_empty() {
                String::new()
            } else {
                format!(" {}", roles.join(" "))
            };
            out.push_str(&format!("- state {:?}{suffix}\n", state.label));
        }
        let mut transitions = self.transitions.iter().collect::<Vec<_>>();
        transitions.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        for transition in transitions {
            out.push_str(&format!("- {}\n", transition.sentence()));
        }
        out
    }

    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let initials = self
            .states
            .iter()
            .filter(|state| state.initial)
            .collect::<Vec<_>>();
        match initials.as_slice() {
            [] => problems.push(Problem {
                code: "missing_initial_state",
                ids: vec![self.id.clone()],
                detail: format!("state machine {:?}: no state is marked initial", self.title),
            }),
            [first, second, ..] => problems.push(Problem {
                code: "multiple_initial_states",
                ids: vec![self.id.clone(), first.id.clone(), second.id.clone()],
                detail: format!(
                    "state machine {:?}: {:?} and {:?} are both marked initial",
                    self.title, first.label, second.label
                ),
            }),
            [_] => {}
        }

        if let Some(initial) = initials.first() {
            let reachable = self.reachable_from(&initial.id);
            for state in self
                .states
                .iter()
                .filter(|state| !reachable.contains(&state.id))
            {
                problems.push(Problem { code: "unreachable_state",
                    ids: vec![self.id.clone(), state.id.clone()],
                    detail: format!(
                        "state machine {:?}: state {:?} is unreachable; no transition reaches it from {:?}",
                        self.title, state.label, initial.label
                    ),
                });
            }
        }

        for state in self.states.iter().filter(|state| state.terminal) {
            for transition in self
                .transitions
                .iter()
                .filter(|transition| transition.from == state.id)
            {
                problems.push(Problem { code: "terminal_transition",
                    ids: vec![self.id.clone(), state.id.clone(), transition.id.clone()],
                    detail: format!(
                        "state machine {:?}: terminal state {:?} has an outgoing transition on {:?}; a terminal state ends the machine",
                        self.title, state.label, transition.event
                    ),
                });
            }
        }

        let mut unguarded: std::collections::BTreeMap<(&str, &str), Vec<&TransitionView>> =
            std::collections::BTreeMap::new();
        for transition in self
            .transitions
            .iter()
            .filter(|transition| transition.guard.is_empty())
        {
            unguarded
                .entry((&transition.from, &transition.event))
                .or_default()
                .push(transition);
        }
        for ((from, event), transitions) in unguarded {
            if transitions.len() < 2 {
                continue;
            }
            let label = self
                .states
                .iter()
                .find(|state| state.id == from)
                .map_or(from, |state| state.label.as_str());
            let mut ids = vec![self.id.clone(), from.to_string()];
            ids.extend(transitions.iter().map(|transition| transition.id.clone()));
            problems.push(Problem { code: "ambiguous_transition",
                ids,
                detail: format!(
                    "state machine {:?}: state {:?} has two transitions on {:?} and neither carries a guard; which one fires is ambiguous",
                    self.title, label, event
                ),
            });
        }

        for transition in self
            .transitions
            .iter()
            .filter(|transition| transition.to_machine != self.id)
        {
            let destination = if transition.to_machine.is_empty() {
                "outside any state machine".to_string()
            } else {
                format!("into state machine {:?}", transition.to_machine_label)
            };
            problems.push(Problem {
                code: "cross_machine_transition",
                ids: vec![self.id.clone(), transition.id.clone()],
                detail: format!(
                    "state machine {:?}: transition {} crosses {destination}",
                    self.title,
                    transition.sentence()
                ),
            });
        }
        problems
    }

    pub fn reachable_from(&self, initial: &str) -> std::collections::BTreeSet<String> {
        let mut reachable = std::collections::BTreeSet::from([initial.to_string()]);
        let mut pending = std::collections::VecDeque::from([initial.to_string()]);
        while let Some(from) = pending.pop_front() {
            for transition in self
                .transitions
                .iter()
                .filter(|transition| transition.from == from && transition.to_machine == self.id)
            {
                if reachable.insert(transition.to.clone()) {
                    pending.push_back(transition.to.clone());
                }
            }
        }
        reachable
    }

    pub fn is_cyclic(&self) -> bool {
        self.states.iter().any(|origin| {
            let mut visited = std::collections::BTreeSet::new();
            let mut pending = self
                .transitions
                .iter()
                .filter(|transition| {
                    transition.from == origin.id && transition.to_machine == self.id
                })
                .map(|transition| transition.to.clone())
                .collect::<Vec<_>>();
            while let Some(at) = pending.pop() {
                if at == origin.id {
                    return true;
                }
                if !visited.insert(at.clone()) {
                    continue;
                }
                pending.extend(
                    self.transitions
                        .iter()
                        .filter(|transition| {
                            transition.from == at && transition.to_machine == self.id
                        })
                        .map(|transition| transition.to.clone()),
                );
            }
            false
        })
    }
}

impl TransitionView {
    pub fn sentence(&self) -> String {
        let guard = if self.guard.is_empty() {
            String::new()
        } else {
            format!(" when {}", self.guard)
        };
        format!(
            "from {:?} on {}{guard} -> {:?}",
            self.from_label, self.event, self.to_label
        )
    }
}
