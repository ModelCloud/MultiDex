//! Model-family presentation at constrained sizes and with configured list bindings.

use super::*;
use codex_models_manager::model_info::multidex_model_catalog;
use pretty_assertions::assert_eq;

fn deepseek_model_presets() -> Vec<ModelPreset> {
    let mut presets: Vec<ModelPreset> = multidex_model_catalog()
        .models
        .into_iter()
        .map(Into::into)
        .collect();
    presets.retain(|preset| {
        codex_model_provider_info::is_multidex_model(&preset.model)
            || codex_model_provider_info::is_openrouter_model(&preset.model)
            || codex_model_provider_info::is_lithos_model(&preset.model)
    });
    presets
}

#[tokio::test]
async fn model_picker_compact_hint_and_actions_follow_configured_bindings() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.accept = vec![key_hint::plain(KeyCode::F(3))];
    keymap.list.cancel = vec![key_hint::plain(KeyCode::F(2))];
    chat.bottom_pane.set_keymap_bindings(&keymap);
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 40);
    assert_eq!(popup.lines().last().unwrap().trim(), "f3 select · f2 back");
    chat.handle_key_event(KeyEvent::from(KeyCode::F(3)));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenReasoningPopup { .. }));
    chat.handle_key_event(KeyEvent::from(KeyCode::F(2)));
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn multi_provider_family_collapses_and_drills_into_variant_then_reasoning() {
    let presets = deepseek_model_presets();
    assert_eq!(presets.len(), 5);
    let (mut chat, mut events, _ops) = make_chatwidget_manual(Some("gpt-5.5")).await;
    Arc::make_mut(&mut chat.model_catalog).models = presets;

    // The five DeepSeek presets collapse into a single family row.
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_eq!(popup.matches("DeepSeek v4.1 Flash").count(), 1);
    assert!(!popup.contains("(Local)"));
    assert!(!popup.contains("(OpenRouter)"));
    assert!(!popup.contains("(Lithos"));

    chat.handle_key_event(KeyCode::Enter.into());
    let member_presets = assert_matches!(
        events.try_recv(),
        Ok(AppEvent::OpenProviderPopup { presets }) => presets
    );
    assert_eq!(member_presets.len(), 5);

    chat.open_provider_popup(member_presets);
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(popup.contains("Local"));
    assert!(popup.contains("OpenRouter"));
    assert!(popup.contains("LithosAI"));

    // Single-preset providers continue straight to the reasoning step.
    chat.handle_key_event(KeyCode::Enter.into());
    let selected = assert_matches!(
        events.try_recv(),
        Ok(AppEvent::OpenReasoningPopup { model }) => model
    );
    assert_eq!(selected.model, "DS v4.1 Flash 1017");
    chat.open_reasoning_popup(selected);
    chat.handle_key_event(KeyCode::Esc.into());

    // LithosAI has three presets and expands into a variant step.
    chat.handle_key_event(KeyCode::Down.into());
    chat.handle_key_event(KeyCode::Down.into());
    chat.handle_key_event(KeyCode::Enter.into());
    let variants = assert_matches!(
        events.try_recv(),
        Ok(AppEvent::OpenVariantPopup { presets }) => presets
    );
    assert_eq!(variants.len(), 3);

    chat.open_variant_popup(variants);
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(popup.contains("Regular"));
    assert!(popup.contains("Fast"));
    assert!(popup.contains("Ultra"));

    chat.handle_key_event(KeyCode::Enter.into());
    let selected = assert_matches!(
        events.try_recv(),
        Ok(AppEvent::OpenReasoningPopup { model }) => model
    );
    assert_eq!(selected.model, "deepseek-ai/DeepSeek-V4.1-Flash");

    // Accepting the reasoning step dismisses the whole family → provider →
    // variant → reasoning chain.
    chat.open_reasoning_popup(selected);
    chat.handle_key_event(KeyCode::Enter.into());
    assert_matches!(events.try_recv(), Ok(AppEvent::UpdateModel(_)));
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn grouped_model_family_marks_row_current_for_member_model() {
    let (mut chat, _events, _ops) =
        make_chatwidget_manual(Some("deepseek-ai/DeepSeek-V4.1-Flash-ultra")).await;
    Arc::make_mut(&mut chat.model_catalog).models = deepseek_model_presets();
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    let family_line = popup
        .lines()
        .find(|line| line.contains("DeepSeek v4.1 Flash"))
        .expect("family row should be present");
    assert!(family_line.contains("(current)"));
}

#[tokio::test]
async fn single_provider_family_stays_flat() {
    let presets: Vec<ModelPreset> = deepseek_model_presets()
        .into_iter()
        .filter(|preset| codex_model_provider_info::is_multidex_model(&preset.model))
        .collect();
    let (mut chat, _events, _ops) = make_chatwidget_manual(Some("gpt-5.5")).await;
    Arc::make_mut(&mut chat.model_catalog).models = presets;
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(popup.contains("DeepSeek v4.1 Flash (Local)"));
    assert!(!popup.contains("Available via"));
}

#[tokio::test]
async fn model_picker_opens_after_discovery_populates_an_empty_catalog() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = std::mem::take(&mut Arc::make_mut(&mut chat.model_catalog).models);
    chat.open_model_popup();
    let loading = render_bottom_popup(&chat, /*width*/ 80);
    let loading_title = loading.lines().find(|line| line.contains("Loading models"));
    insta::assert_snapshot!(loading_title.unwrap().trim(), @"Loading models…");
    let obsolete = chat.model_popup_request_id.unwrap();
    chat.handle_key_event(KeyCode::Esc.into());
    assert!(chat.no_modal_or_popup_active());
    chat.open_model_popup();
    assert!(!chat.on_models_loaded(obsolete, Ok(presets.clone())));
    let request_id = chat.model_popup_request_id.unwrap();
    assert!(chat.on_models_loaded(request_id, Ok(presets)));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_popup", popup);
}
