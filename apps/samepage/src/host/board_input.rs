//! The composed native wait; no wait logic goes into board.wasm.
use super::board::BoardHost;
use ag_ui_surface::{ActionDef, Effect};
use std::{sync::Arc, time::Duration};
pub(crate) fn input_action(
    state: Arc<crate::surface::SamePageState>,
    board: Arc<BoardHost>,
) -> ActionDef {
    let decl = samepage_board::descriptor()
        .actions
        .into_iter()
        .find(|a| a.name == "await_input")
        .expect("input declaration");
    ActionDef::new(decl.name, decl.description, decl.input_schema, move |args|{
        let actor=match ag_ui_surface::current_actor(){Some(actor)=>actor,None=>return Effect::Reject("authenticated input caller is missing".into())};
        let participant=actor.participant_id.unwrap_or_else(||"agent".into());
        let seconds=args["seconds"].as_u64().unwrap_or(60);
        let board=board.clone();let state=state.clone();
        let starting_board=board.revision();
        // A participant's first wait starts from now, not from the page's
        // whole history; page_read is how it learns what is already there.
        state.input.start(&participant,state.host.journal().seq);
        let attention=state.input.attention();
        let starting_attention=attention.as_ref().map(|s|s.human_attention_revision()).unwrap_or(0);
        Effect::AsyncQuery(Box::pin(async move {
            let deadline=tokio::time::Instant::now()+Duration::from_secs(seconds);
            loop {
                let board_woken=board.changed().notified();let doc_woken=state.input.changed.notified();
                tokio::pin!(board_woken,doc_woken);board_woken.as_mut().enable();doc_woken.as_mut().enable();
                let attention_woken=async { match &attention {Some(a)=>a.changed().notified().await,None=>std::future::pending::<()>().await} };
                // Pin/enrol the actual attention Notified before predicates too.
                let attention_notification=attention.as_ref().map(|a|Box::pin(a.changed().notified()));
                let mut attention_notification=attention_notification;
                if let Some(n)=&mut attention_notification { n.as_mut().enable(); }
                let generation=state.validation.generation();
                let validation_woken=state.validation.wait_since(generation,seconds);
                tokio::pin!(validation_woken);
                let check_state=state.clone();let reader=participant.clone();
                let read=tokio::task::spawn_blocking(move ||->Result<Option<String>,String>{
                    let _operation=check_state.durable_operation()?;
                    let view=check_state.host.view();let validation=check_state.validation.report(&view)?;
                    // A page problem wakes each waiter once, when the set of
                    // problems changes. The same problems never wake it again,
                    // so one overlapping card cannot put every agent in a loop.
                    let failed=validation["status"]=="failed";
                    let signature=if failed {crate::surface::validation_signature(&validation)} else {String::new()};
                    let new_problems=check_state.input.report_problems(&reader,signature);
                    let journal=check_state.host.journal();
                    let since=check_state.input.seen(&reader);
                    let changed=journal.recent.iter().any(|e|e.seq>since && e.actor.participant!=reader);
                    if !new_problems && !changed { return Ok(None); }
                    let mut text=String::new();
                    if new_problems {
                        text.push_str("woken by: new page problems\n");
                        text.push_str(&crate::surface::validation_text(&validation));
                        text.push_str("\nnext: repair the ids named above if they are yours, or ask in chat_reply. This wait will not wake again for these same problems.\n");
                    }
                    if changed {
                        drop(view);
                        let readback=check_state.wake_text(&reader,since)?;
                        if !text.is_empty() {text.push('\n');}
                        text.push_str(&readback);
                        if failed && !new_problems {
                            text.push('\n');
                            text.push_str(&crate::surface::validation_text(&validation));
                        }
                    }
                    Ok(Some(text))
                }).await.map_err(|e|format!("input worker failed: {e}"))??;
                if board.revision()!=starting_board {return board.read(&participant);}
                if let Some(read)=read {return Ok(read);}
                if attention.as_ref().is_some_and(|s|s.human_attention_revision()!=starting_attention) {
                    return Ok(attention.as_ref().unwrap().describe());
                }
                let attention_wait=async { match &mut attention_notification {Some(n)=>n.as_mut().await,None=>attention_woken.await} };
                tokio::select!{_ = board_woken.as_mut()=>{},_ = doc_woken.as_mut()=>{},_ = attention_wait=>{},_ = validation_woken.as_mut()=>{},_ = tokio::time::sleep_until(deadline)=>return Ok(format!("Waited {seconds}s and no board, document, attention or chat input arrived. Call await_input again to keep waiting."))}
            }
        }))
    }).agent_only().wake_on_chat()
}
