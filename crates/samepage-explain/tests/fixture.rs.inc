    fn explanation_fixture(a: &str, b: &str) -> ExplanationFlowDefinition {
        ExplanationFlowDefinition {
            about: vec![],
            scene_ids: vec![],
            schema: EXPLANATION_FLOW_SCHEMA.to_string(),
            id: "distance-check".to_string(),
            title: "Distance check".to_string(),
            goal: "Decide from visible evidence whether a candidate is inside.".to_string(),
            start: "observe".to_string(),
            beats: vec![
                ExplanationBeat {
                    id: "observe".to_string(),
                    title: "Observe the comparison".to_string(),
                    intent: "Connect the displayed inequality to the classification.".to_string(),
                    cue: "The boundary is included because the comparison uses less than or equal."
                        .to_string(),
                    evidence: vec![ExplanationEvidence {
                        target_id: a.to_string(),
                        detail: "The displayed distance is no greater than epsilon.".to_string(),
                    }],
                    actions: vec![ExplanationVisualAction {
                        kind: "point".to_string(),
                        target_id: a.to_string(),
                        cue_span: Some([4, 12]),
                    }],
                    advance: ExplanationAdvance {
                        mode: "agent".to_string(),
                        prompt: "Is it inside or beyond?".to_string(),
                        transitions: vec![
                            ExplanationTransition {
                                id: "inside".to_string(),
                                label: "Inside".to_string(),
                                control: Some("button".to_string()),
                                next: Some("contrast".to_string()),
                                target_ids: vec![a.to_string()],
                                phrases: vec!["inside".to_string(), "yes".to_string()],
                            },
                            ExplanationTransition {
                                id: "beyond".to_string(),
                                label: "Beyond".to_string(),
                                control: Some("button".to_string()),
                                next: Some("contrast".to_string()),
                                target_ids: vec![b.to_string()],
                                phrases: vec!["beyond".to_string(), "no".to_string()],
                            },
                        ],
                    },
                },
                ExplanationBeat {
                    id: "contrast".to_string(),
                    title: "Contrast the other case".to_string(),
                    intent: "Transfer the same rule to a second visible case.".to_string(),
                    cue: "Now compare the second candidate with the same threshold.".to_string(),
                    evidence: vec![ExplanationEvidence {
                        target_id: b.to_string(),
                        detail: "The second candidate supplies new visible evidence.".to_string(),
                    }],
                    actions: vec![ExplanationVisualAction {
                        kind: "reveal".to_string(),
                        target_id: b.to_string(),
                        cue_span: None,
                    }],
                    advance: ExplanationAdvance {
                        mode: "terminal".to_string(),
                        prompt: String::new(),
                        transitions: Vec::new(),
                    },
                },
            ],
        }
    }

