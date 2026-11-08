//! `--selftest=<name>`: run one headless test, print one result line, exit with its verdict. Called
//! from `main()` before any window exists, so nothing here can lean on a page to vouch for it
//! (Docs/DESIGN_Pet.md section 8: Rust decides, the page only reports facts).
//!
//! The line is `SelfTest: PASS|FAIL <name>: <detail>`, the repo's existing shape (ChatCore, the UE
//! tests, Scripts/*.ps1), not the `SelfTest: <name> PASS|FAIL` that DESIGN_Pet.md writes.
//!
//! Two guards against a green that proves nothing. A case that ran no checks fails, and a name that
//! is not registered fails with the list of names that are.

use std::ffi::OsString;
use std::fmt::Debug;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vrcue::{
    Binding, Bindings, CharacterAction, Channel, Cue, CueDisposition, CueKind, Degradation, Evidence, Measure,
    Origin, ParamLane, Span,
};

use crate::core::{
    CoreState, EVENT as CORE_EVENT, EXE_ENV, STATE_EVENT as CORE_STATE_EVENT, bad_line_event, candidates, exit_why,
    locate, parse_line, pump, say_line,
};
use crate::cue::resolve::{Claim, ScheduledAction, schedule};
use crate::cast;
use crate::cue::{align, plan, profile};
use crate::fleet::asking_line;
use crate::hit::{self, Hit, Region, Regions};
use crate::press::{self, Held, MAX_HOLD};
use crate::settings::{self, Mode, Settings};
use crate::stt::{self, Op, SttState};
use crate::voice;

/// How many failures of one case are spelled out on the result line.
const FAILURES_SHOWN: usize = 3;

struct Checks {
    run: usize,
    failures: Vec<String>,
}

impl Checks {
    fn that(&mut self, ok: bool, what: impl Into<String>) {
        self.run += 1;
        if !ok {
            self.failures.push(what.into());
        }
    }

    fn eq<T: PartialEq + Debug>(&mut self, got: T, want: T, what: impl Into<String>) {
        self.run += 1;
        if got != want {
            self.failures.push(format!("{}: expected {want:?}, saw {got:?}", what.into()));
        }
    }
}

type Case = (&'static str, fn(&mut Checks));

const TESTS: &[(&str, &[Case])] = &[
    (
        "pet_vocab",
        &[
            ("all_nine_once_in_order", all_nine_once_in_order),
            ("all_covers_every_declared_variant", all_covers_every_declared_variant),
            ("index_round_trips_against_all", index_round_trips_against_all),
            ("origin_matches_design", origin_matches_design),
            ("tags_match_measured_table", tags_match_measured_table),
            ("tags_unique_across_kinds", tags_unique_across_kinds),
            ("span_is_total_and_matches_cpp", span_is_total_and_matches_cpp),
            ("occupies_matches_design", occupies_matches_design),
            ("bindings_default_one_unbound_slot_per_kind", bindings_default_one_unbound_slot_per_kind),
            ("bindings_serde_round_trips", bindings_serde_round_trips),
            ("bindings_missing_key_is_unbound", bindings_missing_key_is_unbound),
        ],
    ),
    (
        "pet_page",
        &[
            ("page_is_the_page", page_is_the_page),
            ("page_never_builds_markup_from_a_string", page_never_builds_markup_from_a_string),
            ("page_builds_nodes_with_text_content", page_builds_nodes_with_text_content),
            ("only_one_file_draws_the_figure", only_one_file_draws_the_figure),
            ("the_character_loads_before_the_page_uses_it", the_character_loads_before_the_page_uses_it),
            ("every_layer_the_rig_names_is_in_the_art", every_layer_the_rig_names_is_in_the_art),
            ("only_the_irises_follow_the_eyeballs", only_the_irises_follow_the_eyeballs),
            ("the_overlay_never_takes_focus_by_itself", the_overlay_never_takes_focus_by_itself),
            ("the_dialogue_has_a_root_for_each_place", the_dialogue_has_a_root_for_each_place),
            ("chat_js_is_the_dialogue_box", chat_js_is_the_dialogue_box),
            ("the_page_and_rust_agree_on_set_regions", the_page_and_rust_agree_on_set_regions),
            ("the_dialogue_is_laid_out_like_a_visual_novel", the_dialogue_is_laid_out_like_a_visual_novel),
            ("every_button_on_the_page_is_reported", every_button_on_the_page_is_reported),
            ("the_arrow_pages_through_an_answer_and_then_opens_the_history", the_arrow_pages_through_an_answer_and_then_opens_the_history),
            ("her_voice_turns_the_page", her_voice_turns_the_page),
            ("the_icon_and_the_paper_are_files_of_their_own", the_icon_and_the_paper_are_files_of_their_own),
            ("the_fleet_list_is_behind_a_button_that_counts", the_fleet_list_is_behind_a_button_that_counts),
            ("the_fixture_is_gone_and_the_page_asks_the_core", the_fixture_is_gone_and_the_page_asks_the_core),
            ("the_page_and_rust_agree_on_the_core", the_page_and_rust_agree_on_the_core),
            ("the_bubble_shows_words_and_not_gesture_markers", the_bubble_shows_words_and_not_gesture_markers),
            (
                "the_core_wiring_comes_after_the_character_and_is_wrapped",
                the_core_wiring_comes_after_the_character_and_is_wrapped,
            ),
            ("the_webview_has_no_way_to_start_a_process", the_webview_has_no_way_to_start_a_process),
            ("the_handler_asks_the_rule_and_the_pet_is_one_window", the_handler_asks_the_rule_and_the_pet_is_one_window),
            ("a_later_part_goes_to_the_queue_and_not_to_speak", a_later_part_goes_to_the_queue_and_not_to_speak),
            ("the_bubble_joins_a_part_the_way_the_core_does", the_bubble_joins_a_part_the_way_the_core_does),
            ("the_bubble_refuses_a_part_for_an_answered_turn", the_bubble_refuses_a_part_for_an_answered_turn),
            ("the_report_says_how_each_part_of_the_last_turn_sounded", the_report_says_how_each_part_of_the_last_turn_sounded),
            ("the_pet_can_be_quit_from_its_own_window", the_pet_can_be_quit_from_its_own_window),
            ("she_is_chosen_from_the_bundled_characters", she_is_chosen_from_the_bundled_characters),
        ],
    ),
    (
        "pet_core",
        &[
            ("an_utterance_line_comes_through_whole", an_utterance_line_comes_through_whole),
            ("both_voices_come_through_whole", both_voices_come_through_whole),
            ("an_error_line_comes_through", an_error_line_comes_through),
            ("every_other_event_is_forwarded_too", every_other_event_is_forwarded_too),
            ("a_line_that_is_not_an_event_is_not_the_end_of_the_loop", a_line_that_is_not_an_event_is_not_the_end_of_the_loop),
            ("a_bad_line_reaches_the_page_as_an_error_it_already_handles", a_bad_line_reaches_the_page_as_an_error_it_already_handles),
            ("the_say_line_is_one_json_line_carrying_the_text_exactly", the_say_line_is_one_json_line_carrying_the_text_exactly),
            ("the_state_follows_the_core_and_down_is_final", the_state_follows_the_core_and_down_is_final),
            ("the_state_reaches_the_page_under_the_names_it_reads", the_state_reaches_the_page_under_the_names_it_reads),
            ("the_core_is_looked_for_where_the_design_says", the_core_is_looked_for_where_the_design_says),
            ("the_home_the_hook_writes_is_the_one_the_pet_reads", the_home_the_hook_writes_is_the_one_the_pet_reads),
            ("the_search_says_what_it_tried", the_search_says_what_it_tried),
            ("an_exit_says_its_code", an_exit_says_its_code),
            ("a_voiced_utterance_gains_a_plan_and_keeps_every_field", a_voiced_utterance_gains_a_plan_and_keeps_every_field),
            ("a_part_number_is_accepted_and_forwarded", a_part_number_is_accepted_and_forwarded),
            ("the_character_and_greet_lines_are_the_cores_wire", the_character_and_greet_lines_are_the_cores_wire),
        ],
    ),
    (
        "pet_hit",
        &[
            ("the_character_rectangle_hits_where_the_spike_did", the_character_rectangle_hits_where_the_spike_did),
            ("edges_are_half_open_where_the_floats_are_exact", edges_are_half_open_where_the_floats_are_exact),
            ("the_gap_between_two_regions_passes_through", the_gap_between_two_regions_passes_through),
            ("the_panel_wins_where_it_overlaps_the_character", the_panel_wins_where_it_overlaps_the_character),
            ("nothing_reported_passes_through", nothing_reported_passes_through),
            ("the_hit_test_makes_no_dpi_assumption", the_hit_test_makes_no_dpi_assumption),
            ("validate_rejects_what_cannot_be_a_region", validate_rejects_what_cannot_be_a_region),
            ("validate_accepts_real_geometry", validate_accepts_real_geometry),
            ("a_report_is_taken_whole_or_not_at_all", a_report_is_taken_whole_or_not_at_all),
            ("the_cursor_reaches_the_page_in_its_own_pixels", the_cursor_reaches_the_page_in_its_own_pixels),
        ],
    ),
    (
        "pet_asking",
        &[
            ("asking_keeps_an_ordinary_message", asking_keeps_an_ordinary_message),
            ("asking_is_one_line", asking_is_one_line),
            ("asking_drops_control_characters", asking_drops_control_characters),
            ("asking_nothing_is_no_line", asking_nothing_is_no_line),
            ("asking_is_bounded_and_says_so", asking_is_bounded_and_says_so),
            ("asking_truncates_on_a_character_boundary", asking_truncates_on_a_character_boundary),
            ("asking_reaches_the_page_under_the_name_it_reads", asking_reaches_the_page_under_the_name_it_reads),
        ],
    ),
    (
        "pet_cues",
        &[
            ("overlapping_hands_earlier_stroke_survives", overlapping_hands_earlier_stroke_survives),
            ("suppressed_cue_reserves_nothing", suppressed_cue_reserves_nothing),
            ("separated_hands_both_play", separated_hands_both_play),
            ("nod_and_look_away_ignore_overlap", nod_and_look_away_ignore_overlap),
            ("interval_is_half_open_at_fall", interval_is_half_open_at_fall),
            ("hold_does_not_widen_interval", hold_does_not_widen_interval),
            ("equal_stroke_earlier_input_wins", equal_stroke_earlier_input_wins),
            ("a_beat_never_takes_a_gesture_slot", a_beat_never_takes_a_gesture_slot),
            ("unbound_is_unresolved_with_no_action", unbound_is_unresolved_with_no_action),
            ("declared_absent_is_unavailable_with_a_nothing_action", declared_absent_is_unavailable_with_a_nothing_action),
            ("degraded_binding_is_reported_and_still_acts", degraded_binding_is_reported_and_still_acts),
            ("cues_with_no_motion_reserve_nothing", cues_with_no_motion_reserve_nothing),
            ("generated_sets_keep_every_invariant", generated_sets_keep_every_invariant),
        ],
    ),
    (
        "pet_filler",
        &[
            ("the_bank_is_present_and_every_filler_is_short", the_bank_is_present_and_every_filler_is_short),
            ("envelope_has_one_frame_per_chunk_and_is_not_all_zero", envelope_has_one_frame_per_chunk_and_is_not_all_zero),
            ("pcm_is_the_length_the_sample_count_implies", pcm_is_the_length_the_sample_count_implies),
            ("round_robin_visits_every_filler_before_repeating", round_robin_visits_every_filler_before_repeating),
            ("an_empty_bank_picks_nothing", an_empty_bank_picks_nothing),
            ("the_page_plays_on_send_and_stops_on_an_answer", the_page_plays_on_send_and_stops_on_an_answer),
            ("the_answer_plays_through_the_same_player", the_answer_plays_through_the_same_player),
            ("later_parts_are_scheduled_back_to_back_on_the_audio_clock", later_parts_are_scheduled_back_to_back_on_the_audio_clock),
            (
                "a_later_part_is_dropped_when_its_turn_was_stopped_or_replaced",
                a_later_part_is_dropped_when_its_turn_was_stopped_or_replaced,
            ),
            ("a_bank_file_is_checked_against_how_the_page_plays_it", a_bank_file_is_checked_against_how_the_page_plays_it),
            ("each_character_plays_the_fillers_the_design_gives_her", each_character_plays_the_fillers_the_design_gives_her),
            ("a_bank_is_read_once_per_shown_character", a_bank_is_read_once_per_shown_character),
        ],
    ),
    (
        "pet_mouth",
        &[
            (
                "the_mouth_is_written_to_the_parameter_the_rig_names",
                the_mouth_is_written_to_the_parameter_the_rig_names,
            ),
            (
                "the_mouth_is_written_last_before_the_figure_is_drawn",
                the_mouth_is_written_last_before_the_figure_is_drawn,
            ),
            (
                "the_page_hands_the_character_a_source_that_follows_the_clip",
                the_page_hands_the_character_a_source_that_follows_the_clip,
            ),
            ("rust_and_the_page_agree_on_the_frame_length", rust_and_the_page_agree_on_the_frame_length),
            ("the_loudness_is_shaped_by_the_face_and_not_in_the_envelope", the_loudness_is_shaped_by_the_face_and_not_in_the_envelope),
            ("the_mouth_reads_the_envelope_of_the_part_sounding_now", the_mouth_reads_the_envelope_of_the_part_sounding_now),
        ],
    ),
    (
        "pet_gesture",
        &[
            ("the_alignment_matches_the_hand_worked_case", the_alignment_matches_the_hand_worked_case),
            ("the_plan_matches_the_cpp_worked_by_hand", the_plan_matches_the_cpp_worked_by_hand),
            ("beats_come_from_the_loudness_peaks", beats_come_from_the_loudness_peaks),
            ("the_same_reply_always_plans_the_same", the_same_reply_always_plans_the_same),
            ("glances_keep_apart_and_to_one_side", glances_keep_apart_and_to_one_side),
            ("resolve_sizes_nods_and_glances_by_the_cue", resolve_sizes_nods_and_glances_by_the_cue),
            ("the_profile_binds_every_kind_to_what_the_rig_has", the_profile_binds_every_kind_to_what_the_rig_has),
            ("a_time_before_the_clip_is_clamped_not_dropped", a_time_before_the_clip_is_clamped_not_dropped),
            ("a_silent_turn_carries_no_plan", a_silent_turn_carries_no_plan),
            ("a_tag_keeps_the_hands_from_a_question_mark", a_tag_keeps_the_hands_from_a_question_mark),
            ("the_louder_beat_is_placed_first", the_louder_beat_is_placed_first),
            ("the_gestures_are_written_where_physics_sees_them", the_gestures_are_written_where_physics_sees_them),
            ("she_looks_toward_the_cursor_and_a_gesture_still_wins", she_looks_toward_the_cursor_and_a_gesture_still_wins),
            ("the_gesture_blend_eases_in_and_out", the_gesture_blend_eases_in_and_out),
            ("the_page_hands_the_character_the_answers_timeline", the_page_hands_the_character_the_answers_timeline),
            ("the_timeline_shifts_each_part_by_where_it_starts", the_timeline_shifts_each_part_by_where_it_starts),
            ("each_part_is_planned_from_its_own_text_and_audio", each_part_is_planned_from_its_own_text_and_audio),
        ],
    ),
    (
        "pet_mic",
        &[
            ("the_lease_pins_the_window_while_held", the_lease_pins_the_window_while_held),
            ("the_poll_thread_asks_the_lease_first", the_poll_thread_asks_the_lease_first),
            ("the_button_lies_inside_the_text_row", the_button_lies_inside_the_text_row),
            ("a_press_and_every_kind_of_release_reach_rust", a_press_and_every_kind_of_release_reach_rust),
            ("the_service_wire_is_restated_to_the_letter", the_service_wire_is_restated_to_the_letter),
            ("the_service_is_looked_for_beside_the_core", the_service_is_looked_for_beside_the_core),
            ("the_character_stops_before_the_microphone_opens", the_character_stops_before_the_microphone_opens),
            ("a_transcript_takes_the_typed_path_once", a_transcript_takes_the_typed_path_once),
        ],
    ),
    (
        "pet_characters",
        &[
            ("a_profile_keeps_to_its_limits", a_profile_keeps_to_its_limits),
            ("an_outfit_names_her_look_in_the_list_only", an_outfit_names_her_look_in_the_list_only),
            ("the_bundled_profile_says_who_she_is_and_no_rules", the_bundled_profile_says_who_she_is_and_no_rules),
            ("an_id_follows_the_folder_rule", an_id_follows_the_folder_rule),
            ("every_user_folder_is_loaded_or_says_why", every_user_folder_is_loaded_or_says_why),
            ("a_chosen_user_character_survives_a_restart_and_falls_back", a_chosen_user_character_survives_a_restart_and_falls_back),
            ("the_scheme_serves_only_checked_files", the_scheme_serves_only_checked_files),
            ("the_core_can_be_told_who_she_is", the_core_can_be_told_who_she_is),
            ("the_page_draws_and_names_what_rust_gives_it", the_page_draws_and_names_what_rust_gives_it),
            ("the_core_hears_who_she_is_before_anything_else", the_core_hears_who_she_is_before_anything_else),
            ("her_greeting_is_voiced_once_and_shown_once", her_greeting_is_voiced_once_and_shown_once),
            ("a_users_fillers_come_from_her_own_folder", a_users_fillers_come_from_her_own_folder),
        ],
    ),
    (
        "pet_settings",
        &[
            ("the_defaults_are_the_designs", the_defaults_are_the_designs),
            ("each_range_holds_its_ends_and_refuses_past_them", each_range_holds_its_ends_and_refuses_past_them),
            ("a_broken_field_falls_back_alone_and_says_why", a_broken_field_falls_back_alone_and_says_why),
            ("a_custom_speech_service_is_checked", a_custom_speech_service_is_checked),
            ("the_file_round_trips_and_keeps_what_it_does_not_know", the_file_round_trips_and_keeps_what_it_does_not_know),
            ("an_old_files_figure_is_the_window", an_old_files_figure_is_the_window),
            ("a_request_is_whole_or_refused", a_request_is_whole_or_refused),
            ("each_window_gets_exactly_its_commands", each_window_gets_exactly_its_commands),
            ("each_page_asks_only_for_what_its_window_may", each_page_asks_only_for_what_its_window_may),
            ("the_settings_page_and_rust_agree_on_every_field", the_settings_page_and_rust_agree_on_every_field),
            ("the_form_sends_every_change_in_order", the_form_sends_every_change_in_order),
            ("the_window_opens_on_the_last_tab", the_window_opens_on_the_last_tab),
            ("the_pet_applies_each_setting_it_is_sent", the_pet_applies_each_setting_it_is_sent),
            ("rust_applies_its_share_and_sends_the_rest", rust_applies_its_share_and_sends_the_rest),
            ("the_window_grows_from_its_bottom_left_corner", the_window_grows_from_its_bottom_left_corner),
            ("the_characters_folder_is_made_when_opened", the_characters_folder_is_made_when_opened),
            ("a_change_of_model_restarts_the_child_that_reads_it", a_change_of_model_restarts_the_child_that_reads_it),
            ("a_restart_closes_stdin_waits_and_kills_what_stays", a_restart_closes_stdin_waits_and_kills_what_stays),
            ("the_packs_offered_are_the_ones_the_service_verified", the_packs_offered_are_the_ones_the_service_verified),
            ("a_users_profile_is_written_by_the_scans_rules", a_users_profile_is_written_by_the_scans_rules),
            ("a_bundled_character_is_copied_to_a_folder_of_her_own", a_bundled_character_is_copied_to_a_folder_of_her_own),
            ("autostart_is_a_startup_shortcut_that_runs_start_pet", autostart_is_a_startup_shortcut_that_runs_start_pet),
            ("the_window_stays_inside_the_work_area", the_window_stays_inside_the_work_area),
            ("the_settings_window_comes_above_the_pet", the_settings_window_comes_above_the_pet),
        ],
    ),
    (
        "pet_themes",
        &[
            ("a_theme_file_keeps_to_its_rules", a_theme_file_keeps_to_its_rules),
            ("every_user_theme_folder_is_loaded_or_says_why", every_user_theme_folder_is_loaded_or_says_why),
            ("the_bundle_ships_paper_and_night", the_bundle_ships_paper_and_night),
            ("a_missing_theme_falls_back_and_says_why", a_missing_theme_falls_back_and_says_why),
            ("the_theme_scheme_serves_only_checked_files", the_theme_scheme_serves_only_checked_files),
            ("the_page_takes_values_and_never_css", the_page_takes_values_and_never_css),
            ("a_switch_applies_without_reloading", a_switch_applies_without_reloading),
        ],
    ),
];

/// Returns the process exit code when the command line asks for a self-test, `None` when it does
/// not and the app should start normally.
pub fn dispatch(args: impl IntoIterator<Item = OsString>) -> Option<i32> {
    let asked: Vec<OsString> =
        args.into_iter().filter(|a| a.to_string_lossy().starts_with("--selftest")).collect();
    if asked.is_empty() {
        return None;
    }
    let (label, result) = match name_asked(&asked) {
        Ok(name) => (name.clone(), run(&name)),
        Err(why) => ("selftest".to_string(), Err(why)),
    };
    Some(match result {
        Ok(detail) => {
            println!("SelfTest: PASS {label}: {detail}");
            0
        }
        Err(why) => {
            println!("SelfTest: FAIL {label}: {why}");
            1
        }
    })
}

fn name_asked(asked: &[OsString]) -> Result<String, String> {
    let [arg] = asked else {
        return Err(format!("expected exactly one --selftest=<name> argument, got {}", asked.len()));
    };
    let Some(arg) = arg.to_str() else {
        return Err("the --selftest argument is not valid UTF-8".to_string());
    };
    match arg.strip_prefix("--selftest=") {
        Some("") => Err("--selftest= has no name".to_string()),
        Some(name) => Ok(name.to_string()),
        None => Err(format!("expected --selftest=<name>, got `{arg}`")),
    }
}

fn run(name: &str) -> Result<String, String> {
    let Some((_, cases)) = TESTS.iter().find(|(n, _)| *n == name) else {
        let known: Vec<&str> = TESTS.iter().map(|(n, _)| *n).collect();
        return Err(format!("unknown selftest `{name}`; known: {}", known.join(", ")));
    };
    let mut checks_run = 0;
    let mut failed: Vec<String> = Vec::new();
    for (case, body) in *cases {
        let mut checks = Checks { run: 0, failures: Vec::new() };
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| body(&mut checks))) {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("(no message)");
            checks.failures.push(format!("panicked: {message}"));
        }
        if checks.run == 0 {
            checks.failures.push("ran no checks, so it proves nothing".to_string());
        }
        checks_run += checks.run;
        if !checks.failures.is_empty() {
            let more = checks.failures.len().saturating_sub(FAILURES_SHOWN);
            let mut shown: Vec<String> = checks.failures.into_iter().take(FAILURES_SHOWN).collect();
            if more > 0 {
                shown.push(format!("+{more} more"));
            }
            failed.push(format!("{case}: {}", shown.join("; ").replace(['\r', '\n'], " ")));
        }
    }
    if failed.is_empty() {
        Ok(format!("{} cases, {checks_run} checks", cases.len()))
    } else {
        Err(format!("{} of {} cases failed | {}", failed.len(), cases.len(), failed.join(" | ")))
    }
}

// ---------------------------------------------------------------------------------------------
// pet_vocab

/// The names the conversation core is meant to print as its vocabulary. The core does not exist
/// yet, so this literal stands in for its list. Adding a kind turns this red until it is updated,
/// which is the point (Docs/DESIGN_Pet.md 1.3 step 4).
const VOCABULARY: [&str; 9] = ["selfRef", "offer", "open", "question", "beat", "beatBoth", "rest", "nod", "lookAway"];

fn wire_name(kind: CueKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn all_nine_once_in_order(c: &mut Checks) {
    let names: Vec<String> = CueKind::ALL.iter().map(|&k| wire_name(k)).collect();
    c.eq(names, VOCABULARY.map(String::from).to_vec(), "CueKind::ALL by wire name");
    for (i, kind) in CueKind::ALL.iter().enumerate() {
        c.that(!CueKind::ALL[..i].contains(kind), format!("{kind:?} appears in ALL more than once"));
    }
}

/// `ALL` is an array literal with its length written out, so adding a variant and forgetting it
/// there still compiles. serde's unknown-variant error lists the variants the enum declares.
fn all_covers_every_declared_variant(c: &mut Checks) {
    let message = match serde_json::from_str::<CueKind>("\"?\"") {
        Err(e) => e.to_string(),
        Ok(kind) => format!("parsed `?` as {kind:?}"),
    };
    let declared: Vec<&str> = message.split('`').skip(3).step_by(2).collect();
    c.that(!declared.is_empty(), format!("could not read the declared variants out of: {message}"));
    for name in &declared {
        c.that(
            CueKind::ALL.iter().any(|&k| wire_name(k) == *name),
            format!("variant `{name}` is declared but missing from CueKind::ALL"),
        );
    }
    c.eq(declared.len(), CueKind::ALL.len(), "declared variants against ALL.len()");
}

fn index_round_trips_against_all(c: &mut Checks) {
    for (i, &kind) in CueKind::ALL.iter().enumerate() {
        c.eq(kind.index(), i, format!("{kind:?}.index()"));
        c.eq(CueKind::ALL.get(kind.index()).copied(), Some(kind), format!("ALL[{kind:?}.index()]"));
    }
}

fn origin_matches_design(c: &mut Checks) {
    use CueKind::*;
    use Origin::*;
    let want = [
        (SelfRef, Semantic),
        (Offer, Semantic),
        (Open, Semantic),
        (Question, Semantic),
        (Beat, Prosodic),
        (BeatBoth, Prosodic),
        (Rest, Baseline),
        (Nod, Semantic),
        (LookAway, Semantic),
    ];
    c.eq(want.len(), CueKind::ALL.len(), "the expected table covers every kind");
    for (kind, origin) in want {
        c.eq(kind.origin(), origin, format!("{kind:?}.origin()"));
    }
}

fn tags_match_measured_table(c: &mut Checks) {
    use CueKind::*;
    let want: [(CueKind, &[&str]); 9] = [
        (SelfRef, &["self"]),
        (Offer, &["you", "offer"]),
        (Open, &["open"]),
        (Question, &["shrug", "question"]),
        (Beat, &[]),
        (BeatBoth, &[]),
        (Rest, &[]),
        (Nod, &["nod"]),
        (LookAway, &["think"]),
    ];
    c.eq(want.len(), CueKind::ALL.len(), "the expected table covers every kind");
    for (kind, tags) in want {
        c.eq(kind.tags(), tags, format!("{kind:?}.tags()"));
    }
}

fn tags_unique_across_kinds(c: &mut Checks) {
    let mut owner: Vec<(&str, CueKind)> = Vec::new();
    for kind in CueKind::ALL {
        for &tag in kind.tags() {
            if let Some(&(_, first)) = owner.iter().find(|(t, _)| *t == tag) {
                c.that(false, format!("tag `{tag}` is under both {first:?} and {kind:?}"));
            }
            // VRGesturePlanner lower-cases and trims a tag before it compares (VRGesturePlan.cpp:82).
            c.that(
                !tag.is_empty() && tag == tag.trim().to_lowercase(),
                format!("tag `{tag}` under {kind:?} could never match a lower-cased, trimmed tag"),
            );
            owner.push((tag, kind));
        }
    }
    c.that(!owner.is_empty(), "no tags were seen at all");
}

fn span_is_total_and_matches_cpp(c: &mut Checks) {
    use CueKind::*;
    for kind in CueKind::ALL {
        let Span { rise, hold, fall } = kind.span();
        c.that(
            [rise, hold, fall].iter().all(|v| v.is_finite() && *v >= 0.0),
            format!("{kind:?}.span() has a value that is not a finite, non-negative number"),
        );
        if kind.occupies().exclusive() {
            c.that(rise > 0.0 && fall > 0.0, format!("{kind:?} holds the hands but reserves no interval"));
        }
    }
    // VRGesturePlan.cpp:22.
    let hand = Span { rise: 0.45, hold: 0.25, fall: 0.75 };
    let beat = Span { rise: 0.3, hold: 0.08, fall: 0.5 };
    for kind in [SelfRef, Offer, Open, Question] {
        c.eq(kind.span(), hand, format!("{kind:?}.span()"));
    }
    for kind in [Beat, BeatBoth] {
        c.eq(kind.span(), beat, format!("{kind:?}.span()"));
    }
    // VRBodyLanguageComponent.cpp:28, NodHalf either side of the deepest point.
    c.eq(Nod.span(), Span { rise: 0.3, hold: 0.0, fall: 0.3 }, "Nod.span()");
}

fn occupies_matches_design(c: &mut Checks) {
    use CueKind::*;
    let want = [
        (SelfRef, Channel::Hands),
        (Offer, Channel::Hands),
        (Open, Channel::Hands),
        (Question, Channel::Hands),
        (Beat, Channel::Hands),
        (BeatBoth, Channel::Hands),
        (Rest, Channel::Baseline),
        (Nod, Channel::Head),
        (LookAway, Channel::Eyes),
    ];
    c.eq(want.len(), CueKind::ALL.len(), "the expected table covers every kind");
    for (kind, channel) in want {
        c.eq(kind.occupies(), channel, format!("{kind:?}.occupies()"));
        c.eq(kind.occupies().exclusive(), channel == Channel::Hands, format!("{kind:?} exclusivity"));
    }
}

fn bindings_default_one_unbound_slot_per_kind(c: &mut Checks) {
    let b = Bindings::default();
    c.eq(b.iter().count(), CueKind::ALL.len(), "slots in a default Bindings");
    c.eq(b.iter().map(|(k, _)| k).collect::<Vec<_>>(), CueKind::ALL.to_vec(), "slot order against ALL");
    for kind in CueKind::ALL {
        c.eq(&b[kind], &Binding::Unbound, format!("default slot {kind:?}"));
    }
}

fn motion(group: &str) -> CharacterAction {
    CharacterAction::PlayMotion { group: group.to_string(), index: 1, fade_in: 0.25 }
}

/// One slot in each of the five states, and all three actions among them.
fn populated() -> Bindings {
    let mut b = Bindings::default();
    b.set(CueKind::SelfRef, Binding::Guessed { action: motion("self"), pattern: "self".to_string() });
    b.set(
        CueKind::Offer,
        Binding::Chosen {
            action: CharacterAction::DriveParams {
                lanes: vec![ParamLane {
                    id: "ParamAngleY".to_string(),
                    to: 7.5,
                    per: None,
                    rise: 0.3,
                    hold: 0.0,
                    fall: 0.3,
                    lag: 0.0,
                    relative: false,
                }],
            },
            evidence: Evidence::ExactFingerprintProfile,
        },
    );
    b.set(
        CueKind::BeatBoth,
        Binding::Degraded { action: motion("beat"), why: Degradation::PlaysAs(CueKind::Beat) },
    );
    b.set(CueKind::Question, Binding::DeclaredAbsent);
    b.set(CueKind::Rest, Binding::Chosen { action: CharacterAction::Nothing, evidence: Evidence::AuthoredProfile });
    b
}

fn bindings_serde_round_trips(c: &mut Checks) {
    let before = populated();
    let wire = match serde_json::to_value(&before) {
        Ok(v) => v,
        Err(e) => return c.that(false, format!("serializing: {e}")),
    };
    let mut keys: Vec<&str> = wire.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    keys.sort_unstable();
    let mut want: Vec<&str> = VOCABULARY.to_vec();
    want.sort_unstable();
    c.eq(keys, want, "keys written, Unbound slots included");
    match serde_json::from_value::<Bindings>(wire) {
        Ok(after) => c.eq(after, before, "Bindings after a round trip"),
        Err(e) => c.that(false, format!("deserializing what was just written: {e}")),
    }
}

fn bindings_missing_key_is_unbound(c: &mut Checks) {
    let populated = populated();
    let Ok(mut wire) = serde_json::to_value(&populated) else {
        return c.that(false, "serializing the populated Bindings");
    };

    // A file written before `question` existed: the key is simply not there.
    if let Some(object) = wire.as_object_mut() {
        object.remove("question");
    }
    match serde_json::from_value::<Bindings>(wire) {
        Ok(b) => {
            c.eq(b.iter().count(), CueKind::ALL.len(), "slots after reading an object missing a key");
            c.eq(&b[CueKind::Question], &Binding::Unbound, "the missing key");
            c.eq(&b[CueKind::SelfRef], &populated[CueKind::SelfRef], "a key that was present");
            c.eq(&b[CueKind::BeatBoth], &populated[CueKind::BeatBoth], "another key that was present");
        }
        Err(e) => c.that(false, format!("an object missing a key should still read: {e}")),
    }

    match serde_json::from_str::<Bindings>("{}") {
        Ok(b) => {
            c.eq(b.iter().count(), CueKind::ALL.len(), "slots after reading {}");
            c.that(b.iter().all(|(_, s)| *s == Binding::Unbound), "every slot of {} is Unbound");
        }
        Err(e) => c.that(false, format!("{{}} should read as all Unbound: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------
// pet_cues

fn plain(kind: CueKind, at: f32) -> Cue {
    Cue::plain(kind, at).expect("the test built a cue the constructor refuses")
}

fn nod(at: f32) -> Cue {
    Cue::nod(at, 3.0).expect("the test built a cue the constructor refuses")
}

fn look_away(at: f32, end: f32) -> Cue {
    Cue::look_away(at, end, 10.0, -8.0).expect("the test built a cue the constructor refuses")
}

/// The action a kind is bound to in these tests, recognisable by its group name.
fn action_of(kind: CueKind) -> CharacterAction {
    motion(&format!("{kind:?}"))
}

fn chosen(kind: CueKind) -> Binding {
    Binding::Chosen { action: action_of(kind), evidence: Evidence::UserPreviewed }
}

fn all_chosen() -> Bindings {
    let mut b = Bindings::default();
    for kind in CueKind::ALL {
        b.set(kind, chosen(kind));
    }
    b
}

/// What the page would be told to do, as `(at, group)`; "nothing" for the do-nothing action.
fn plan(actions: &[ScheduledAction]) -> Vec<(f32, String)> {
    actions
        .iter()
        .map(|a| match &a.action {
            CharacterAction::PlayMotion { group, .. } => (a.at, group.clone()),
            CharacterAction::DriveParams { .. } => (a.at, "params".to_string()),
            CharacterAction::Nothing => (a.at, "nothing".to_string()),
        })
        .collect()
}

fn played(kind: CueKind) -> CueDisposition {
    CueDisposition::Played { kind }
}

fn suppressed(kind: CueKind, by: CueKind) -> CueDisposition {
    CueDisposition::Suppressed { kind, by }
}

/// The claim these cases give a cue of `kind`: a tag for anything the words ask for, and a beat of
/// no particular loudness, so beats among themselves go by input position. `pet_gesture` covers the
/// two claims these cases do not use, a question mark and a beat louder than another.
fn claim_of(kind: CueKind) -> Claim {
    match kind.origin() {
        Origin::Prosodic => Claim::Stress(0.0),
        Origin::Semantic | Origin::Baseline => Claim::Tag,
    }
}

/// `schedule`, plus the checks that hold for every input: one disposition per cue, in input order,
/// and the actions ordered by stroke time.
fn run_schedule(
    c: &mut Checks,
    cues: &[Cue],
    b: &Bindings,
) -> (Vec<ScheduledAction>, Vec<CueDisposition>) {
    let claimed: Vec<(Cue, Claim)> = cues.iter().map(|&cue| (cue, claim_of(cue.kind()))).collect();
    let (actions, dispositions) = schedule(&claimed, b);
    c.eq(dispositions.len(), cues.len(), "dispositions against input cues");
    for (i, (cue, d)) in cues.iter().zip(&dispositions).enumerate() {
        c.eq(d.kind(), cue.kind(), format!("disposition {i} is about input cue {i}"));
    }
    c.that(actions.windows(2).all(|w| w[0].at <= w[1].at), "actions are ordered by stroke time");
    (actions, dispositions)
}

fn overlapping_hands_earlier_stroke_survives(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();
    // Offer [0.55, 1.75) and Open [1.05, 2.25) intersect.
    let (actions, d) = run_schedule(c, &[plain(Offer, 1.0), plain(Open, 1.5)], &b);
    c.eq(d, vec![played(Offer), suppressed(Open, Offer)], "later stroke listed second");
    c.eq(plan(&actions), vec![(1.0, "Offer".to_string())], "actions, later stroke listed second");

    let (actions, d) = run_schedule(c, &[plain(Open, 1.5), plain(Offer, 1.0)], &b);
    c.eq(d, vec![suppressed(Open, Offer), played(Offer)], "later stroke listed first");
    c.eq(plan(&actions), vec![(1.0, "Offer".to_string())], "actions, later stroke listed first");
}

/// A suppressed cue never reached the stage, so it holds no hands. Offer at 1.0 covers [0.55, 1.75),
/// Open at 1.5 is suppressed, and Question at 2.3 [1.85, 3.05) clears Offer and only overlaps Open.
fn suppressed_cue_reserves_nothing(c: &mut Checks) {
    use CueKind::*;
    let (actions, d) =
        run_schedule(c, &[plain(Offer, 1.0), plain(Open, 1.5), plain(Question, 2.3)], &all_chosen());
    c.eq(d, vec![played(Offer), suppressed(Open, Offer), played(Question)], "dispositions");
    c.eq(plan(&actions), vec![(1.0, "Offer".to_string()), (2.3, "Question".to_string())], "actions");
}

fn separated_hands_both_play(c: &mut Checks) {
    use CueKind::*;
    let (actions, d) = run_schedule(c, &[plain(Open, 3.0), plain(Offer, 1.0)], &all_chosen());
    c.eq(d, vec![played(Open), played(Offer)], "dispositions, in input order");
    c.eq(plan(&actions), vec![(1.0, "Offer".to_string()), (3.0, "Open".to_string())], "actions, by stroke time");
}

fn nod_and_look_away_ignore_overlap(c: &mut Checks) {
    use CueKind::*;
    let cues = [
        plain(Offer, 2.0),
        nod(2.0),
        look_away(2.0, 3.0),
        nod(2.1),
        look_away(2.5, 3.5),
        plain(Rest, 2.0),
    ];
    let (actions, d) = run_schedule(c, &cues, &all_chosen());
    c.eq(d, vec![played(Offer), played(Nod), played(LookAway), played(Nod), played(LookAway), played(Rest)], "dispositions");
    c.eq(actions.len(), cues.len(), "every one of them has an action");
}

/// A stroke time `t` for which `t - rise` is exactly `end` in f32, the arithmetic `schedule` uses.
/// `end + rise` is only close: the sum rounds, and so does the subtraction back.
fn stroke_starting_at(end: f32, rise: f32) -> Option<f32> {
    let guess = (end + rise).to_bits();
    (0..=16u32)
        .flat_map(|step| [guess + step, guess.saturating_sub(step)])
        .map(f32::from_bits)
        .find(|t| t - rise == end)
}

fn interval_is_half_open_at_fall(c: &mut Checks) {
    use CueKind::*;
    let first = plain(Offer, 2.0);
    let end = 2.0 + Offer.span().fall;
    let rise = Open.span().rise;
    let Some(exactly) = stroke_starting_at(end, rise) else {
        return c.that(false, "could not build a stroke that starts exactly where the first one ends");
    };
    let just_inside = f32::from_bits(exactly.to_bits() - 1);
    c.that(just_inside - rise < end, "the next float down must start before the first one ends");

    let b = all_chosen();
    let (_, d) = run_schedule(c, &[first, plain(Open, exactly)], &b);
    c.eq(d, vec![played(Offer), played(Open)], "second starts exactly at first's t + fall");
    let (_, d) = run_schedule(c, &[first, plain(Open, just_inside)], &b);
    c.eq(d, vec![played(Offer), suppressed(Open, Offer)], "second starts one float inside");
}

fn hold_does_not_widen_interval(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();
    for (a, next) in [(Offer, Open), (Beat, BeatBoth)] {
        let (span_a, span_next) = (a.span(), next.span());
        let end = 2.0 + span_a.fall;
        // Clear of the first one's real interval, but inside it if `hold` were added to `fall`.
        let clear = end + 0.8 * span_a.hold + span_next.rise;
        let (_, d) = run_schedule(c, &[plain(a, 2.0), plain(next, clear)], &b);
        c.eq(d, vec![played(a), played(next)], format!("{next:?} starting {:.3}s after {a:?}'s fall", 0.8 * span_a.hold));
        // The same pair, closer than the interval, so the test above cannot pass for lack of overlap.
        let (_, d) = run_schedule(c, &[plain(a, 2.0), plain(next, end - 0.05 + span_next.rise)], &b);
        c.eq(d, vec![played(a), suppressed(next, a)], format!("{next:?} starting 0.05s before {a:?}'s fall ends"));
    }
}

fn equal_stroke_earlier_input_wins(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();
    let (_, d) = run_schedule(c, &[plain(Open, 1.0), plain(Offer, 1.0)], &b);
    c.eq(d, vec![played(Open), suppressed(Offer, Open)], "Open listed first");
    let (_, d) = run_schedule(c, &[plain(Offer, 1.0), plain(Open, 1.0)], &b);
    c.eq(d, vec![played(Offer), suppressed(Open, Offer)], "Offer listed first");
}

/// The planner places every cue the words asked for before any beat, because a beat takes
/// "whatever room the words leave" (VRGesturePlan.cpp:362, sort at :378-385). Scheduling by stroke
/// time alone let a beat at 1.0 s suppress an `Offer` at 1.2 s, which is a meaningful gesture lost
/// to filler. Without this case a revert to stroke order passes every other check here.
fn a_beat_never_takes_a_gesture_slot(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();

    // The beat strikes first and still loses, which is the whole point.
    let (actions, d) = run_schedule(c, &[plain(Beat, 1.0), plain(Offer, 1.2)], &b);
    c.eq(d, vec![suppressed(Beat, Offer), played(Offer)], "a beat ahead of a gesture it overlaps");
    c.eq(plan(&actions), vec![(1.2, "Offer".to_string())], "only the gesture is played");

    // And it still loses the other way round, where stroke order already agreed.
    let (_, d) = run_schedule(c, &[plain(Offer, 1.0), plain(Beat, 1.2)], &b);
    c.eq(d, vec![played(Offer), suppressed(Beat, Offer)], "a beat behind a gesture it overlaps");

    // A beat that collides with nothing keeps its place: priority decides collisions, not whether
    // a beat is allowed at all.
    let (actions, d) = run_schedule(c, &[plain(Beat, 0.5), plain(Offer, 3.0)], &b);
    c.eq(d, vec![played(Beat), played(Offer)], "a beat clear of every gesture");

    // Placement order must not leak into the order the page is given. The beat is placed second
    // and strikes first, so these two orders disagree, and the page gets stroke order.
    c.eq(
        plan(&actions),
        vec![(0.5, "Beat".to_string()), (3.0, "Offer".to_string())],
        "the page is given stroke order, not placement order",
    );
}

fn unbound_is_unresolved_with_no_action(c: &mut Checks) {
    use CueKind::*;
    let mut b = Bindings::default();
    b.set(Offer, chosen(Offer));
    let (actions, d) = run_schedule(c, &[plain(Offer, 1.0), plain(Open, 3.0)], &b);
    c.eq(d, vec![played(Offer), CueDisposition::Unresolved { kind: Open }], "dispositions");
    c.eq(plan(&actions), vec![(1.0, "Offer".to_string())], "only the bound cue has an action");

    let (actions, d) = run_schedule(c, &[plain(Open, 3.0), nod(1.0)], &Bindings::default());
    c.eq(d, vec![CueDisposition::Unresolved { kind: Open }, CueDisposition::Unresolved { kind: Nod }], "nothing bound");
    c.eq(actions.len(), 0, "actions when nothing is bound");
}

fn declared_absent_is_unavailable_with_a_nothing_action(c: &mut Checks) {
    use CueKind::*;
    let mut b = Bindings::default();
    b.set(Offer, Binding::DeclaredAbsent);
    let (actions, d) = run_schedule(c, &[plain(Offer, 1.0)], &b);
    c.eq(d, vec![CueDisposition::Unavailable { kind: Offer }], "disposition");
    c.eq(plan(&actions), vec![(1.0, "nothing".to_string())], "the action is the deliberate Nothing");
}

fn degraded_binding_is_reported_and_still_acts(c: &mut Checks) {
    use CueKind::*;
    let why = Degradation::PlaysAs(Beat);
    let mut b = all_chosen();
    b.set(BeatBoth, Binding::Degraded { action: action_of(Beat), why });
    let (actions, d) = run_schedule(c, &[plain(BeatBoth, 1.0)], &b);
    c.eq(d, vec![CueDisposition::Degraded { kind: BeatBoth, why }], "disposition");
    c.eq(plan(&actions), vec![(1.0, "Beat".to_string())], "it plays what it is bound to");

    // Losing the overlap is a different story from being degraded: it did not play at all.
    let (_, d) = run_schedule(c, &[plain(Offer, 1.0), plain(BeatBoth, 1.2)], &b);
    c.eq(d, vec![played(Offer), suppressed(BeatBoth, Offer)], "a degraded cue that loses the overlap");
}

fn cues_with_no_motion_reserve_nothing(c: &mut Checks) {
    use CueKind::*;
    let mut b = all_chosen();

    b.set(Open, Binding::Unbound);
    let (_, d) = run_schedule(c, &[plain(Open, 1.0), plain(Offer, 1.2)], &b);
    c.eq(d, vec![CueDisposition::Unresolved { kind: Open }, played(Offer)], "an unbound cue ahead of a gesture");

    b.set(Open, Binding::DeclaredAbsent);
    let (actions, d) = run_schedule(c, &[plain(Open, 1.0), plain(Offer, 1.2)], &b);
    c.eq(
        d,
        vec![CueDisposition::Unavailable { kind: Open }, played(Offer)],
        "a declared-absent cue ahead of a gesture",
    );
    c.eq(
        plan(&actions),
        vec![(1.0, "nothing".to_string()), (1.2, "Offer".to_string())],
        "actions with a declared-absent cue ahead of a gesture",
    );

    let (_, d) = run_schedule(c, &[plain(Offer, 1.0), plain(Open, 1.2)], &b);
    c.eq(
        d,
        vec![played(Offer), CueDisposition::Unavailable { kind: Open }],
        "a declared-absent cue behind a gesture is not suppressed",
    );
}

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u32) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as u32) % n
    }
}

/// The spec restated as an oracle, run over many generated cue sets. The collision test repeats
/// the interval formula on purpose: it is the specification, and the case above pins its edge.
fn generated_sets_keep_every_invariant(c: &mut Checks) {
    let mut rng = Lcg(0x5EED_CAFE);
    let mut seen = [0usize; 5]; // played, degraded, unavailable, suppressed, unresolved
    for set in 0..400 {
        let mut b = Bindings::default();
        for kind in CueKind::ALL {
            b.set(
                kind,
                match rng.below(6) {
                    0 => Binding::Unbound,
                    1 => Binding::DeclaredAbsent,
                    2 => Binding::Degraded { action: action_of(kind), why: Degradation::PlaysAs(CueKind::Beat) },
                    _ => chosen(kind),
                },
            );
        }
        let cues: Vec<Cue> = (0..rng.below(13))
            .map(|_| {
                let kind = CueKind::ALL[rng.below(9) as usize];
                let at = rng.below(120) as f32 * 0.05;
                match kind {
                    CueKind::Nod => nod(at),
                    CueKind::LookAway => look_away(at, at + 0.5),
                    _ => plain(kind, at),
                }
            })
            .collect();
        let (actions, d) = run_schedule(c, &cues, &b);

        let moves_hands = |i: usize| {
            cues[i].kind().occupies().exclusive() && !matches!(b[cues[i].kind()], Binding::DeclaredAbsent)
        };
        // Reaching the schedule means producing an action. A declared-absent cue does, because its
        // deliberate `Nothing` is what keeps the page's third branch reachable; an unbound one does
        // not, because nobody has said what it would play.
        let reached = |i: usize| {
            matches!(
                d[i],
                CueDisposition::Played { .. } | CueDisposition::Degraded { .. } | CueDisposition::Unavailable { .. }
            )
        };
        let collide = |i: usize, j: usize| {
            let (a, z) = (cues[i].kind().span(), cues[j].kind().span());
            cues[i].at() - a.rise < cues[j].at() + z.fall && cues[j].at() - z.rise < cues[i].at() + a.fall
        };
        // Placement order, not stroke order: a semantic cue is placed before any beat however late
        // it strikes (cue/resolve.rs `Claim`, VRGesturePlan.cpp:378-385), and beats of one loudness
        // go by input position. Written as the same three-part key the scheduler sorts by, so a
        // change to one has to change both.
        let place = |i: usize| match claim_of(cues[i].kind()) {
            Claim::Tag => (0u8, cues[i].at()),
            Claim::QuestionMark => (1, cues[i].at()),
            Claim::Stress(loudness) => (2, -loudness),
        };
        let before = |j: usize, i: usize| (place(j).0, place(j).1, j) < (place(i).0, place(i).1, i);

        let mut kept: Vec<usize> = (0..cues.len()).filter(|&i| reached(i)).collect();
        kept.sort_by(|&x, &y| cues[x].at().total_cmp(&cues[y].at()).then(x.cmp(&y)));
        c.eq(actions.len(), kept.len(), format!("set {set}: one action per cue that reached the schedule"));
        c.that(
            kept.iter().zip(&actions).all(|(&i, a)| cues[i].at() == a.at),
            format!("set {set}: actions line up with the cues that reached the schedule, by stroke time"),
        );

        for i in 0..cues.len() {
            let unbound = matches!(b[cues[i].kind()], Binding::Unbound);
            match d[i] {
                CueDisposition::Unresolved { .. } => {
                    seen[4] += 1;
                    c.that(unbound, format!("set {set}: cue {i} is Unresolved but its binding is not Unbound"));
                }
                CueDisposition::Unavailable { .. } => {
                    seen[2] += 1;
                    c.that(
                        matches!(b[cues[i].kind()], Binding::DeclaredAbsent),
                        format!("set {set}: cue {i} is Unavailable but the user never declared it absent"),
                    );
                }
                CueDisposition::Suppressed { by, .. } => {
                    seen[3] += 1;
                    c.that(moves_hands(i), format!("set {set}: cue {i} is Suppressed but holds no hands"));
                    c.that(
                        (0..cues.len()).any(|j| {
                            j != i && reached(j) && moves_hands(j) && cues[j].kind() == by && before(j, i) && collide(j, i)
                        }),
                        format!("set {set}: cue {i} is Suppressed by {by:?}, which did not hold the hands over it"),
                    );
                }
                CueDisposition::Degraded { .. } => {
                    seen[1] += 1;
                    c.that(
                        matches!(b[cues[i].kind()], Binding::Degraded { .. }),
                        format!("set {set}: cue {i} is Degraded but its binding is not"),
                    );
                }
                CueDisposition::Played { .. } => {
                    seen[0] += 1;
                    c.that(
                        !unbound
                            && !matches!(
                                b[cues[i].kind()],
                                Binding::Degraded { .. } | Binding::DeclaredAbsent
                            ),
                        format!("set {set}: cue {i} is Played but its binding says otherwise"),
                    );
                }
            }
            if !moves_hands(i) && !unbound {
                c.that(reached(i), format!("set {set}: cue {i} moves no hands yet did not reach the schedule"));
            }
            for j in (i + 1)..cues.len() {
                if reached(i) && reached(j) && moves_hands(i) && moves_hands(j) {
                    c.that(!collide(i, j), format!("set {set}: cues {i} and {j} both hold the hands and overlap"));
                }
            }
        }
    }
    for (count, name) in seen.iter().zip(["Played", "Degraded", "Unavailable", "Suppressed", "Unresolved"]) {
        c.that(*count > 0, format!("the generated sets never produced a {name}, so they prove nothing about it"));
    }
}

// -------------------------------------------------------------------------------------------
// pet_asking: what a roster row may show about what a session is asking (DESIGN_Pet.md 5.3)

/// The cap is written out here on purpose rather than imported. It is a product decision about how
/// much of one session's message may push the other nineteen down, so moving it should cost a
/// deliberate edit to this test, not pass quietly.
const ASKING_MAX: usize = 140;

fn asking_keeps_an_ordinary_message(c: &mut Checks) {
    let m = "Claude needs your permission to use Bash";
    c.eq(asking_line(m), Some(m.to_string()), "an ordinary one-line message is shown as it is");
}

fn asking_is_one_line(c: &mut Checks) {
    c.eq(asking_line("a\r\n\tb   c"), Some("a b c".to_string()), "newlines and tabs collapse to one space");
    c.eq(asking_line("   hello"), Some("hello".to_string()), "a leading run of whitespace leaves no leading space");
    c.eq(asking_line("hello   "), Some("hello".to_string()), "a trailing run of whitespace leaves no trailing space");
    let out = asking_line("one\n\n\ntwo").unwrap_or_default();
    c.that(!out.contains("  "), format!("no doubled space survives: {out:?}"));
}

fn asking_drops_control_characters(c: &mut Checks) {
    // An ANSI escape is a control character followed by ordinary letters. The control byte goes and
    // the letters stay, which is honest: we are not pretending to parse terminal colour.
    let out = asking_line("a\u{1b}[31mb\0c\u{7}d").unwrap_or_default();
    c.that(!out.chars().any(char::is_control), format!("no control character survives: {out:?}"));
    c.that(out.contains('a') && out.contains('d'), format!("the readable text survives: {out:?}"));
}

fn asking_nothing_is_no_line(c: &mut Checks) {
    // None and not Some("") so the row shows no line at all. An empty line in the panel reads as a
    // rendering bug, and the product's claim is that a quiet row means nothing is waiting.
    for raw in ["", "   ", "\r\n\t ", "\0\u{1b}"] {
        c.eq(asking_line(raw), None, format!("{raw:?} is no line"));
    }
}

fn asking_is_bounded_and_says_so(c: &mut Checks) {
    let long = "x".repeat(ASKING_MAX * 4);
    let out = asking_line(&long).unwrap_or_default();
    c.eq(out.chars().count(), ASKING_MAX + 1, "a long message is cut to the cap plus one marker");
    c.that(out.ends_with('\u{2026}'), format!("the cut is marked: {out:?}"));
    c.eq(
        out.chars().take(ASKING_MAX).collect::<String>(),
        "x".repeat(ASKING_MAX),
        "the kept part is the start of the message",
    );

    // Exactly at the cap nothing is cut, so the marker never appears on a message that fits.
    let fits = "y".repeat(ASKING_MAX);
    c.eq(asking_line(&fits), Some(fits.clone()), "a message exactly at the cap is untouched");
}

fn asking_truncates_on_a_character_boundary(c: &mut Checks) {
    // Multibyte input, because cutting by bytes would either panic or produce invalid UTF-8, and a
    // message in Chinese is the ordinary case for this user, not an edge case.
    let long = "\u{65e5}".repeat(ASKING_MAX * 2);
    let out = asking_line(&long).unwrap_or_default();
    c.eq(out.chars().count(), ASKING_MAX + 1, "a multibyte message is cut by characters, not bytes");
    c.that(out.len() > out.chars().count(), format!("the output really is multibyte: {} bytes", out.len()));
}

/// The wire names the page reads. This is the case that would have caught the bug this unit fixed:
/// the page read `waitingSince` and `waitingMessage` while Rust wrote `waiting_since` and
/// `waiting_message`, so both were `undefined` and the row silently showed the wrong age and no
/// message at all. Nothing errored, so only an assertion on the key set finds it.
fn asking_reaches_the_page_under_the_name_it_reads(c: &mut Checks) {
    let s = crate::Session {
        pid: 1234,
        cwd: "D:\\log\\VRPhysProto".to_string(),
        kind: "interactive".to_string(),
        started_at: 1_762_000_000,
        session_id: "abc123".to_string(),
        name: Some("pet".to_string()),
        status: "idle".to_string(),
        waiting: true,
        waiting_since: Some(1_762_000_500),
        waiting_message: asking_line("Claude needs your permission to use Bash"),
    };
    let v = serde_json::to_value(&s).unwrap_or(serde_json::Value::Null);
    let mut keys: Vec<String> = v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    keys.sort();
    let mut want: Vec<String> = [
        "pid",
        "cwd",
        "kind",
        "startedAt",
        "sessionId",
        "name",
        "status",
        "waiting",
        "waitingSince",
        "waitingMessage",
    ]
    .iter()
    .map(|k| k.to_string())
    .collect();
    want.sort();
    c.eq(keys, want, "the keys the page reads are the keys Rust writes");
    c.eq(
        v.get("waitingMessage").and_then(|x| x.as_str()).map(str::to_string),
        Some("Claude needs your permission to use Bash".to_string()),
        "the asking line arrives under waitingMessage",
    );
    c.eq(v.get("waitingSince").and_then(|x| x.as_u64()), Some(1_762_000_500), "waitingSince arrives as a number");

    // And the rename must not have broken reading the CLI, which is the other half of this struct.
    let from_cli = r#"{"pid":7,"cwd":"C:\\x","kind":"interactive","startedAt":5,"sessionId":"z","status":"busy"}"#;
    let parsed = serde_json::from_str::<crate::Session>(from_cli);
    c.that(parsed.is_ok(), format!("a `claude agents --json` row still parses: {parsed:?}"));
    if let Ok(p) = parsed {
        c.eq(p.session_id, "z".to_string(), "sessionId still reads");
        c.eq(p.started_at, 5i64, "startedAt still reads");
        c.that(!p.waiting && p.waiting_message.is_none(), "the hook's fields are not read from the CLI");
    }
}

// -------------------------------------------------------------------------------------------
// pet_hit: which part of the overlay the cursor is over, and which geometry reports are believed
// (Docs/DESIGN_Pet.md 5.1). Pure, so it needs no window.

/// The window the way this machine measured it: 460x620 physical. It sits at (100, 50) and not at
/// the origin, so a hit test that forgets the window's position is wrong at every point instead of
/// only at the ones near zero.
const WINDOW_AT: (f64, f64) = (100.0, 50.0);
const WINDOW_SIZE: (f64, f64) = (460.0, 620.0);

/// A region written in the pixels of the 460x620 window, the way the page's layout is written, and
/// stored the way the page reports it: as fractions of the viewport.
fn px(x: f64, y: f64, w: f64, h: f64) -> Region {
    Region { x: x / WINDOW_SIZE.0, y: y / WINDOW_SIZE.1, w: w / WINDOW_SIZE.0, h: h / WINDOW_SIZE.1 }
}

/// What the cursor is over when it is `dx`, `dy` physical pixels into the window.
fn over(r: &Regions, dx: f64, dy: f64) -> Hit {
    hit::hit(r, (WINDOW_AT.0 + dx, WINDOW_AT.1 + dy), WINDOW_AT, WINDOW_SIZE)
}

/// The character as the page lays it out: left 70, bottom 10, 320 square.
fn character_rect() -> Region {
    px(70.0, 290.0, 320.0, 320.0)
}

/// The dialogue's lowest card, which reaches down into the top right of the character's rectangle.
fn card_over_character() -> Region {
    px(225.0, 250.0, 225.0, 80.0)
}

fn the_character_rectangle_hits_where_the_spike_did(c: &mut Checks) {
    let r = Regions { character: vec![character_rect()], panel: vec![] };
    // Half a pixel either side of each edge and not on it: an edge is where float rounding decides
    // which side a point is on, and a real cursor sits on whole pixels anyway.
    for (dx, dy, want, why) in [
        (230.0, 450.0, Hit::Character, "the centre"),
        (70.5, 290.5, Hit::Character, "just inside the top left corner"),
        (389.5, 290.5, Hit::Character, "just inside the top right corner"),
        (70.5, 609.5, Hit::Character, "just inside the bottom left corner"),
        (389.5, 609.5, Hit::Character, "just inside the bottom right corner"),
        (69.5, 450.0, Hit::Through, "just left of the left edge"),
        (390.5, 450.0, Hit::Through, "just right of the right edge"),
        (230.0, 289.5, Hit::Through, "just above the top edge"),
        (230.0, 610.5, Hit::Through, "just below the bottom edge"),
        // The two points Pet/README.md names as the ones the 25% scale bug answered backwards.
        (72.0, 450.0, Hit::Character, "L+72"),
        (400.0, 450.0, Hit::Through, "L+400"),
    ] {
        c.eq(over(&r, dx, dy), want, why);
    }
}

fn edges_are_half_open_where_the_floats_are_exact(c: &mut Checks) {
    // A 512 square viewport and fractions that are powers of two, so every edge below is an exact
    // float and a point can sit precisely on it: the left and top edges belong to the region and
    // the right and bottom edges do not.
    let r = Regions { character: vec![Region { x: 0.25, y: 0.25, w: 0.5, h: 0.5 }], panel: vec![] };
    let at = |x: f64, y: f64| hit::hit(&r, (WINDOW_AT.0 + x, WINDOW_AT.1 + y), WINDOW_AT, (512.0, 512.0));
    c.eq(at(128.0, 200.0), Hit::Character, "exactly on the left edge");
    c.eq(at(200.0, 128.0), Hit::Character, "exactly on the top edge");
    c.eq(at(384.0, 200.0), Hit::Through, "exactly on the right edge");
    c.eq(at(200.0, 384.0), Hit::Through, "exactly on the bottom edge");
    c.eq(at(383.5, 383.5), Hit::Character, "half a pixel inside the bottom right corner");
    c.eq(at(127.5, 200.0), Hit::Through, "half a pixel outside the left edge");
}

fn the_gap_between_two_regions_passes_through(c: &mut Checks) {
    // The silhouette case section 5.1 plans for: several rectangles in one list, and a bounding box
    // around them would swallow the gap.
    let r = Regions { character: vec![px(70.0, 290.0, 100.0, 320.0), px(250.0, 290.0, 140.0, 320.0)], panel: vec![] };
    c.eq(over(&r, 120.0, 450.0), Hit::Character, "inside the first");
    c.eq(over(&r, 320.0, 450.0), Hit::Character, "inside the second");
    c.eq(over(&r, 210.0, 450.0), Hit::Through, "in the gap between them");
}

fn the_panel_wins_where_it_overlaps_the_character(c: &mut Checks) {
    let both = Regions { character: vec![character_rect()], panel: vec![card_over_character()] };
    c.eq(over(&both, 300.0, 310.0), Hit::Panel, "a point in both is the panel's");
    c.eq(over(&both, 300.0, 260.0), Hit::Panel, "a point only in the card is the panel's");
    c.eq(over(&both, 150.0, 450.0), Hit::Character, "a point only in the character is the character's");

    // The controls. The same point answers Character once the card is not in the panel list, so the
    // Panel above comes from the list and not from the geometry.
    let without = Regions { character: vec![character_rect()], panel: vec![] };
    c.eq(over(&without, 300.0, 310.0), Hit::Character, "with no panel the overlap point is the character's");
    let wrong_list = Regions { character: vec![character_rect(), card_over_character()], panel: vec![] };
    c.eq(over(&wrong_list, 300.0, 260.0), Hit::Character, "a card reported as character is a drag, not a click");
}

fn nothing_reported_passes_through(c: &mut Checks) {
    let none = Regions::default();
    for (dx, dy, why) in [
        (230.0, 450.0, "where the character will be"),
        (300.0, 300.0, "where a card will be"),
        (0.0, 0.0, "the window's corner"),
        (-50.0, -50.0, "outside the window"),
    ] {
        c.eq(over(&none, dx, dy), Hit::Through, format!("before any report, {why}"));
    }
    // A closed dialogue reports no panel, and where its card was must stop taking clicks.
    let closed = Regions { character: vec![character_rect()], panel: vec![] };
    c.eq(over(&closed, 300.0, 260.0), Hit::Through, "with the dialogue closed, where its card was");
    // The window is only click-through before the first report if that is what main() starts from.
    c.that(MAIN.contains("manage(Mutex::new(hit::Regions::default()))"), "main() starts from an empty report");
}

fn the_hit_test_makes_no_dpi_assumption(c: &mut Checks) {
    let r = Regions { character: vec![character_rect()], panel: vec![] };
    // The same fractions in a viewport 1.25 times larger, as at 125% display scaling: every point
    // scales with it and nothing is multiplied by a scale factor.
    let big = (WINDOW_SIZE.0 * 1.25, WINDOW_SIZE.1 * 1.25);
    let in_big = |dx: f64, dy: f64| hit::hit(&r, (WINDOW_AT.0 + dx, WINDOW_AT.1 + dy), WINDOW_AT, big);
    c.eq(in_big(72.0 * 1.25, 450.0 * 1.25), Hit::Character, "L+72 scaled by 1.25 is still inside");
    c.eq(in_big(400.0 * 1.25, 450.0 * 1.25), Hit::Through, "L+400 scaled by 1.25 is still outside");
    // And the control: one physical point that the two sizes disagree about, so a hit test that
    // ignored `size` could not pass both lines.
    c.eq(over(&r, 395.0, 450.0), Hit::Through, "x 395 is outside the character at 460 wide");
    c.eq(in_big(395.0, 450.0 * 1.25), Hit::Character, "the same x is inside it at 575 wide");
}

fn validate_rejects_what_cannot_be_a_region(c: &mut Checks) {
    let ok = character_rect();
    for (why, bad) in [
        ("a NaN x", Region { x: f64::NAN, ..ok }),
        ("a NaN width", Region { w: f64::NAN, ..ok }),
        ("an infinite y", Region { y: f64::INFINITY, ..ok }),
        ("a negative infinite height", Region { h: f64::NEG_INFINITY, ..ok }),
        ("a zero width", Region { w: 0.0, ..ok }),
        ("a zero height", Region { h: 0.0, ..ok }),
        ("a negative width", Region { w: -0.1, ..ok }),
        ("a negative height", Region { h: -0.1, ..ok }),
        ("a left edge left of the viewport", Region { x: -0.01, ..ok }),
        ("a top edge above the viewport", Region { y: -0.01, ..ok }),
        ("a right edge one pixel past the viewport", px(225.0, 100.0, 236.0, 50.0)),
        ("a bottom edge one pixel past the viewport", px(70.0, 300.0, 100.0, 321.0)),
    ] {
        c.that(hit::validate(&[bad]).is_err(), format!("{why} is rejected"));
    }

    // One bad rectangle among good ones rejects the list, and the message says which.
    let err = hit::validate(&[ok, Region { w: f64::NAN, ..ok }]).err().unwrap_or_default();
    c.that(err.starts_with("region 1 "), format!("the rejection names the position: {err:?}"));
}

fn validate_accepts_real_geometry(c: &mut Checks) {
    let report = [character_rect(), card_over_character()];
    c.eq(hit::validate(&report), Ok(report.to_vec()), "an ordinary report comes back unchanged");
    c.eq(hit::validate(&[]), Ok(vec![]), "an empty list is valid, because a closed dialogue reports no panel");

    // Where a box anchored to the right edge ends up.
    c.that(hit::validate(&[px(225.0, 100.0, 235.0, 50.0)]).is_ok(), "a region flush with the right edge");
    // One ulp past 1.0, which is what dividing two CSS pixel values can leave behind.
    let noisy = Region { x: 0.5, y: 0.0, w: 0.5 + f64::EPSILON, h: 0.1 };
    c.that(noisy.x + noisy.w > 1.0, "the noisy rectangle really does end past 1.0");
    c.that(hit::validate(&[noisy]).is_ok(), "float noise at the edge is not an overhang");
}

/// The poll reads physical pixels and the page draws in CSS ones. A figure that looked at the
/// physical position would look past the cursor on any screen not at 100 %.
fn the_cursor_reaches_the_page_in_its_own_pixels(c: &mut Checks) {
    c.eq(hit::css((1250.0, 700.0), (1000.0, 500.0), 1.25), hit::Css { x: 200.0, y: 160.0 }, "inside, at 125 %");
    c.eq(hit::css((900.0, 450.0), (1000.0, 500.0), 1.0), hit::Css { x: -100.0, y: -50.0 }, "off the window, past its edge");
    c.eq(hit::css((3000.0, 500.0), (1000.0, 500.0), 2.0), hit::Css { x: 1000.0, y: 0.0 }, "on another monitor to the right");
}

fn a_report_is_taken_whole_or_not_at_all(c: &mut Checks) {
    let character = character_rect();
    let card = card_over_character();
    let bad = Region { w: f64::NAN, ..card };

    let mut current = Regions::default();
    c.that(current.accept(&[character], &[card]).is_ok(), "a valid report is accepted");
    c.eq(current.clone(), Regions { character: vec![character], panel: vec![card] }, "and becomes the current one");

    // The good half of each rejected report differs from what is current, so taking it would show.
    let before = current.clone();
    let other = px(0.0, 0.0, 50.0, 50.0);
    let err = current.accept(&[other], &[card, bad]).err().unwrap_or_default();
    c.that(err.starts_with("panel region 1 "), format!("a bad panel list is named as such: {err:?}"));
    c.eq(current.clone(), before.clone(), "it leaves the character list that was fine as it was too");

    let err = current.accept(&[bad], &[other]).err().unwrap_or_default();
    c.that(err.starts_with("character region 0 "), format!("a bad character list is named as such: {err:?}"));
    c.eq(current, before, "and the panel list that was fine is not taken either");
}

// -------------------------------------------------------------------------------------------
// pet_page: what the page is not allowed to contain. Needs no window, because it reads the file.

/// The page as it will be compiled into the executable. `tauri::generate_context!` embeds this same
/// file from `frontendDist`, so this is the shipped text and not a copy that could drift.
const PAGE: &str = include_str!("../../ui/index.html");

/// The one file that knows how the figure is drawn (Docs/DESIGN_Pet.md 2). Embedded for the same
/// reason as the page: `frontendDist` compiles it into this executable, so this is the shipped text
/// rather than a copy of it that could drift.
const CHARACTER: &str = include_str!("../../ui/character.js");

/// The figure: how she moves, and the layers she is drawn from. Compiled in from `frontendDist` like
/// the rest, so these are the bytes the page fetches and the bytes `cue/profile.rs` fingerprints.
const RIG: &str = include_str!("../../ui/characters/char-20261107/rig.json");
const ART: &str = include_str!("../../ui/characters/char-20261107/layers.json");

/// Which characters the bundle has, which `cast.rs` reads through the asset resolver.
const CAST: &str = include_str!("../../ui/characters/index.json");

/// The dialogue box. It draws text another process or a model wrote, in a page that holds `invoke`,
/// so it is held to the same rules as the page itself.
const CHAT: &str = include_str!("../../ui/chat.js");

/// Where `set_regions` is defined and registered, which the page's `invoke` has to match by name.
const MAIN: &str = include_str!("main.rs");

/// A wrong path would not compile, but an empty or renamed file would let every assertion below
/// pass by being about nothing. This is the case that stops `pet_page` going vacuous.
fn page_is_the_page(c: &mut Checks) {
    c.that(PAGE.len() > 2000, format!("the embedded page has content: {} bytes", PAGE.len()));
    for marker in ["id=\"rows\"", "id=\"panel\"", "__TAURI__"] {
        c.that(PAGE.contains(marker), format!("the embedded file is the pet's page: {marker} is in it"));
    }
}

/// The lines that assign to `innerHTML`. The word also appears in comments that explain this rule,
/// so the sink is an assignment to it and not the word. `===` is a comparison and not a sink.
fn innerhtml_assignments(source: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for (at, _) in source.match_indices("innerHTML") {
        let rest = source[at + "innerHTML".len()..].trim_start();
        let assigned = rest.starts_with('=') && !rest.starts_with("==");
        let appended = rest.starts_with("+=");
        if assigned || appended {
            lines.push(source[..at].matches('\n').count() + 1);
        }
    }
    lines
}

/// Every way a string becomes markup, and the reason this test exists.
///
/// The diag block appended `fleetErr` into `innerHTML` without escaping it, and `fleetErr` is the
/// stderr of the `claude agents --json` process carried through verbatim (`run_json`). The page
/// had an `esc()` helper and a comment claiming everything external went through it. It did not.
/// A guard you have to remember to call is a convention; this is the guard.
fn page_never_builds_markup_from_a_string(c: &mut Checks) {
    // Every file, because the rule is about the page and `character.js` and `chat.js` are parts of
    // it. Splitting code out of index.html would otherwise have quietly halved what this guard
    // covers, which is the way a guard usually stops working.
    for (name, source) in [("index.html", PAGE), ("character.js", CHARACTER), ("chat.js", CHAT), ("settings.html", SETTINGS_PAGE)] {
        let assignments: Vec<String> =
            innerhtml_assignments(source).iter().map(|line| format!("{name} line {line}")).collect();
        c.eq(assignments, Vec::<String>::new(), format!("no assignment to innerHTML in {name}"));

        // The siblings that do the same thing by another name.
        for sink in ["outerHTML", "insertAdjacentHTML", "document.write", "eval("] {
            c.that(!source.contains(sink), format!("{name} does not use {sink}"));
        }
    }
}

/// The seam, asserted from the outside.
///
/// `character.js` is the only file that knows how the figure is drawn (Docs/DESIGN_Pet.md 2). That
/// is a sentence in a document until something checks it, and the cheapest way for it to rot is
/// for someone to draw on the canvas from the page because that is where the canvas is.
fn only_one_file_draws_the_figure(c: &mut Checks) {
    c.that(CHARACTER.len() > 2000, format!("character.js has content: {} bytes", CHARACTER.len()));
    c.that(CHARACTER.contains("canvas.getContext(\"2d\")"), "character.js is the file that draws the figure");
    c.that(CHARACTER.contains("ctx.drawImage(l.image, l.x, l.y);"), "and it draws her from her layers");

    // Two files of our own and nothing third-party. The figure needs no runtime, and the one the
    // Live2D model needed could not have shipped with an open-source pet (Docs/DESIGN_Pet.md 3.4).
    c.eq(PAGE.matches("<script src=").count(), 2, "the page loads two scripts");
    c.that(!PAGE.contains("<script src=\"vendor/"), "and neither of them is a vendored download");

    // A call is the page reaching into the drawing; the word in a comment is not. This rule has to
    // be explainable in a comment, and a lint that forbids its own explanation gets deleted by the
    // next person instead of obeyed.
    c.eq(
        reaches_runtime(PAGE, &["getContext(", "drawImage(", "rig.json"]),
        Vec::<String>::new(),
        "index.html never draws the figure itself",
    );
    c.eq(
        reaches_runtime(CHAT, &["getContext(", "drawImage(", "rig.json"]),
        Vec::<String>::new(),
        "chat.js never draws the figure",
    );
    // And the Cubism runtime does not come back by a side door.
    for (name, source) in [("index.html", PAGE), ("character.js", CHARACTER), ("chat.js", CHAT)] {
        c.eq(reaches_runtime(source, &["PIXI", "Live2D"]), Vec::<String>::new(), format!("{name} never names the Live2D runtime"));
    }
}

/// The lines of `source` that mention one of `needles`, ignoring the comments and script tags that
/// are allowed to explain or load them.
fn reaches_runtime(source: &str, needles: &[&str]) -> Vec<String> {
    let mut reaches = Vec::new();
    for needle in needles {
        for (at, _) in source.match_indices(needle) {
            let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = source[at..].find('\n').map_or(source.len(), |i| at + i);
            let line = source[line_start..line_end].trim();
            if line.starts_with("//") || line.starts_with("<!--") || line.starts_with("<script src=") {
                continue;
            }
            let line_no = source[..at].matches('\n').count() + 1;
            reaches.push(format!("line {line_no}: {}", line.chars().take(60).collect::<String>()));
        }
    }
    reaches
}

/// The markers, which are for the body and not for the eye.
///
/// A gesture arrives in two forms at once: in front of its clause as a marker the Unreal planner
/// parses, and in the event's `tags`. If the page shows the text as it arrived, the person reads
/// the markers, which is what the first real engine did on screen. Nothing else catches it: the
/// markers are legal text to every layer below, and a screenshot is the only other place it shows.
///
/// The opening line is here too because it is the one sentence the page says for itself, and it
/// used to explain that the engine was the echo one. A sentence the page states about the engine
/// goes stale the moment the engine changes, and a character that opens by apologising for
/// something no longer true is worse than one that says nothing.
fn the_bubble_shows_words_and_not_gesture_markers(c: &mut Checks) {
    c.that(!PAGE.contains("text: p.text"), "the bubble is not handed the core's text as it arrived");
    c.that(
        PAGE.contains("const words = spoken(p.text);")
            && PAGE.contains("let text = words;")
            && PAGE.contains("Chat.show({ who: Chat.Who.Character, text, choices: [] });"),
        "it is handed the text with the markers taken out",
    );
    c.that(PAGE.contains("function spoken("), "and the page is where that happens");
    // The gestures still have to reach the page, because the Live2D cues will read them from there.
    c.that(!PAGE.contains("delete p.tags"), "and the gestures are left on the event");

    let opening: String =
        PAGE.find("const OPENING").map(|at| PAGE[at..].chars().take_while(|ch| !ch.is_control()).collect()).unwrap_or_default();
    c.that(!opening.is_empty(), "the page has an opening line");
    for stale in ["echo", "引擎"] {
        c.that(!opening.contains(stale), format!("and it does not talk about the engine ({stale})"));
    }
}

/// The wire that makes the handler consult the rule, and the pet's window as the configuration
/// names it. The rule itself, each window's list, is `pet_settings`.
///
/// Tauri runs its own permission list over a custom command only when the app ships an ACL
/// manifest or the caller is remote, and this app ships neither, so every local window reaches
/// every command unless `refuse` says otherwise. `say` spends the user's subscription, so that is
/// not a tidiness question. A rule nothing calls refuses nothing, which is what this half is for.
///
/// The label itself is checked against `tauri.conf.json` rather than against a copy of the word,
/// because the failure if those two drift is the whole app refusing itself, and it would arrive as
/// a window that answers nothing rather than as an error naming the cause.
fn the_handler_asks_the_rule_and_the_pet_is_one_window(c: &mut Checks) {
    c.that(crate::refuse(crate::OVERLAY, "say").is_none(), "the pet's window may ask for say");
    for label in ["log", "", "Main", "main ", "ma in"] {
        match crate::refuse(label, "say") {
            None => c.that(false, format!("a window called {label:?} should not be able to ask")),
            Some(why) => c.that(why.contains("say"), format!("the refusal for {label:?} names the command: {why}")),
        }
    }

    // The bare form, which is what this file is here to keep out of the builder.
    c.that(
        !MAIN.contains(".invoke_handler(tauri::generate_handler!"),
        "the command list does not reach the builder unguarded",
    );
    c.that(MAIN.contains("refuse(invoke.message.webview_ref().label()"), "the handler asks the rule about the caller");

    // One place spells the label. A second spelling is how the guard and the window stop agreeing.
    c.eq(MAIN.matches("\"main\"").count(), 1, "the label is written once in main.rs");

    // The settings window is built when it is asked for (main.rs `open_settings`), so the
    // configuration still starts the app with one.
    let conf: Value = serde_json::from_str(CONF).unwrap_or(Value::Null);
    let windows = conf.pointer("/app/windows").and_then(Value::as_array).cloned().unwrap_or_default();
    c.eq(windows.len(), 1, "the configuration starts the app with one window");
    let labels: Vec<&str> = windows.iter().filter_map(|w| w.get("label").and_then(Value::as_str)).collect();
    c.eq(labels, vec![crate::OVERLAY], "and it is the pet's");
}

/// Load order, which is not a style question here.
///
/// The inline script calls `Character.mount` and `Chat.mount` as it runs, and a browser runs classic
/// scripts in document order, so the order of these tags is the whole dependency graph and nothing
/// else records it.
fn the_character_loads_before_the_page_uses_it(c: &mut Checks) {
    let mut last = 0;
    for tag in ["<script src=\"character.js\">", "<script src=\"chat.js\">", "<script>"] {
        match PAGE.find(tag) {
            None => c.that(false, format!("the page has {tag}")),
            Some(at) => {
                c.that(at > last, format!("{tag} comes after the one before it"));
                last = at;
            }
        }
    }
}

/// Every layer the rig moves is a layer of the art, and every layer of the art is a PNG in the
/// bundle the size `layers.json` gives it. A rig naming a layer the art lacks fails the figure at
/// mount, a missing PNG leaves her `missing`, and a PNG of another size is drawn out of place, so
/// each of the three is a pet with no character or a broken one. A parameter the rig moves without
/// declaring fails the mount as well, so that is here too, and so does a layer clipped to one that
/// is not drawn before it.
fn every_layer_the_rig_names_is_in_the_art(c: &mut Checks) {
    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let art: Value = serde_json::from_str(ART).unwrap_or(Value::Null);
    c.eq(rig["art"].as_str(), Some("layers.json"), "rig.json is drawn from the layers.json beside it");
    let layers = art["layers"].as_array().cloned().unwrap_or_default();
    c.eq(layers.len(), 22, "the art has its 22 layers");
    let names: Vec<&str> = layers.iter().filter_map(|l| l["name"].as_str()).collect();
    let rigged = rig["layers"].as_object().cloned().unwrap_or_default();
    c.eq(rigged.len(), layers.len(), "the rig moves every layer of the art");
    for name in rigged.keys() {
        c.that(names.contains(&name.as_str()), format!("rig.json moves {name}, which layers.json has"));
    }
    let drawn_at = |name: &str| names.iter().position(|n| *n == name);
    for (name, l) in &rigged {
        if let Some(mask) = l.get("clip") {
            let before = match (mask.as_str().and_then(drawn_at), drawn_at(name)) {
                (Some(m), Some(at)) => m < at,
                _ => false,
            };
            c.that(before, format!("{name} is clipped to {mask}, a layer drawn before it"));
        }
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/characters/char-20261107");
    for l in &layers {
        let file = l["file"].as_str().unwrap_or("");
        let want = (l["w"].as_u64(), l["h"].as_u64());
        match std::fs::read(dir.join(file)) {
            Err(e) => c.that(false, format!("{file}: {e}")),
            Ok(bytes) => {
                let png = bytes.len() >= 24 && bytes[..8] == *b"\x89PNG\r\n\x1a\n";
                let size = |at: usize| u64::from(u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]));
                let got = if png { (Some(size(16)), Some(size(20))) } else { (None, None) };
                c.eq(got, want, format!("{file} is a PNG the size layers.json gives it"));
            }
        }
    }

    let declared: Vec<&str> =
        rig["parameters"].as_array().map(|ps| ps.iter().filter_map(|p| p["id"].as_str()).collect()).unwrap_or_default();
    let mut used: Vec<(String, String)> = Vec::new();
    let mut by = |at: String, list: &Value| {
        for d in list.as_array().into_iter().flatten() {
            used.push((at.clone(), d["param"].as_str().unwrap_or("").to_string()));
        }
    };
    for n in rig["nodes"].as_array().into_iter().flatten() {
        by(format!("node {}", n["name"]), &n["drive"]);
    }
    for (name, l) in &rigged {
        by(format!("layer {name}"), &l["drive"]);
        if let Some(show) = l.get("show") {
            by(format!("layer {name} shows"), &json!([show]));
        }
    }
    by("the idle".to_string(), &rig["idle"]);
    by("the physics".to_string(), &rig["physics"]);
    for h in rig["physics"].as_array().into_iter().flatten() {
        by(format!("the physics of {}", h["param"]), &h["input"]);
    }
    for id in rig["eyeBlink"].as_array().into_iter().flatten() {
        by("the blink".to_string(), &json!([{ "param": id }]));
    }
    c.that(used.len() > 40, format!("the rig moves {} things by parameter", used.len()));
    for (at, id) in &used {
        c.that(declared.contains(&id.as_str()), format!("{at} moves by {id}, which rig.json declares"));
    }
}

/// Looking aside moves the irises and nothing else of the eye: the white and the lashes stay where
/// the face has them, and each iris is clipped to its own white, so it goes under the corners of
/// the eye instead of sliding the whole eye across the face.
fn only_the_irises_follow_the_eyeballs(c: &mut Checks) {
    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let rigged = rig["layers"].as_object().cloned().unwrap_or_default();
    let mut irises = 0;
    for (name, l) in &rigged {
        let by: Vec<&str> = l["drive"].as_array().into_iter().flatten().filter_map(|d| d["param"].as_str()).collect();
        let follows = by.iter().any(|id| *id == "ParamEyeBallX" || *id == "ParamEyeBallY");
        if !name.starts_with("iris_") {
            c.that(!follows, format!("{name} is not an iris, so the eyeballs do not move it"));
            continue;
        }
        irises += 1;
        c.that(
            by.contains(&"ParamEyeBallX") && by.contains(&"ParamEyeBallY"),
            format!("{name} follows the eyeballs on both axes"),
        );
        let white = name.replacen("iris_", "eye_white_", 1);
        c.eq(l["clip"].as_str(), Some(white.as_str()), format!("{name} is clipped to {white}"));
    }
    c.eq(irises, 2, "the art has two irises");
}

/// The helper the rows and the diag line are both built with. If it ever stops using textContent,
/// every caller silently starts accepting markup, and no caller would look different.
fn page_builds_nodes_with_text_content(c: &mut Checks) {
    c.that(PAGE.contains("n.textContent = text"), "el() sets textContent");
    c.that(PAGE.contains("replaceChildren"), "the page replaces children rather than rewriting HTML");
}

/// The overlay may hold a text field, and may never put the caret in it by itself.
///
/// The rule this replaces said the overlay could hold no text field at all, because the design
/// called the window non-activating and a field in such a window would draw, look right and never
/// receive a key. Two things changed on 2026-11-03. The window was measured and it does activate,
/// so the field works. And the user asked for one, so the question stopped being whether to type
/// here and became when the caret may move.
///
/// It may move when the user clicks the field, which the browser does on its own. Anything else is
/// the pet pulling the caret out of whatever the user was typing in elsewhere, and the dialogue
/// redraws on every turn, so an `autofocus` or a `.focus()` call would do exactly that on a timer.
/// `setFocus` is the Tauri window call with the same effect from the other side.
///
/// Folded to lower case because the attribute is `autofocus` and the window method is `setFocus`.
/// Comments are not exempt, so no file can use these words even to explain the rule.
fn the_overlay_never_takes_focus_by_itself(c: &mut Checks) {
    // `.select(` is here because it moves the caret into a field as surely as the others, which
    // was measured in the same engine. The quoted tag names are here because the dialogue builds
    // its nodes with `createElement`, so `<textarea` alone would be a guard about nothing: that
    // was in the rule this one replaced, and dropping it was a regression.
    const FORBIDDEN: &[&str] = &[
        ".focus(",
        ".select(",
        "autofocus",
        "setfocus",
        "<textarea",
        "\"textarea\"",
        "'textarea'",
        "`textarea`",
        "contenteditable",
        "designmode",
    ];
    // Three files, because the overlay is three files. character.js is clean today, so this is a
    // hole being closed rather than a break being fixed.
    for (name, source) in [("index.html", PAGE), ("chat.js", CHAT), ("character.js", CHARACTER)] {
        let lower = source.to_lowercase();
        let mut found = Vec::new();
        for needle in FORBIDDEN {
            for (at, _) in lower.match_indices(needle) {
                found.push(format!("{name} line {} has {needle}", lower[..at].matches('\n').count() + 1));
            }
        }
        c.eq(found, Vec::<String>::new(), format!("nothing in {name} moves the caret by itself"));
    }

    // One field, so the rule above is about something. Counted on the creation call rather than on
    // the markup, because the page has no `<input` anywhere: chat.js builds it.
    c.eq(CHAT.matches("createElement(\"input\")").count(), 1, "chat.js builds exactly one text field");
    c.that(PAGE.matches("<input").count() == 0, "the page markup declares no field of its own");
}

/// The dialogue has four places, the box, the choices, the text field and the history, and the page
/// owns where each one sits (Docs/DESIGN_Pet.md 4.4.6), because where they go is decided by the
/// character's silhouette and `chat.js` has never seen the character. `mount` throws when a root is
/// missing, so this guard is about the page keeping its side of a contract, not about a graceful
/// degradation that does not exist. Where the four sit is `the_dialogue_is_laid_out_like_a_visual_novel`.
fn the_dialogue_has_a_root_for_each_place(c: &mut Checks) {
    for id in ["chatBubble", "chatChoices", "chatInput", "chatLog"] {
        c.that(PAGE.contains(&format!("id=\"{id}\"")), format!("the page has a root for {id}"));
    }
    for key in ["bubble", "choices", "input", "log"] {
        c.that(CHAT.contains(&format!("{key}:")), format!("chat.js names the {key} root"));
    }

    // The character goes up before anything optional is wired, and the optional wiring is
    // wrapped. Both because a throw at load time runs off the end of the script: the first
    // version of the side wiring reached through `__TAURI__.event` above this line, and against
    // a stand-in lacking it the character never mounted at all.
    let mount = PAGE.find("Character.mount(");
    // The needle carries `window.` so it matches the call and not the comment above it that
    // explains the rule. The first version matched the comment and failed on itself.
    let wiring = PAGE.find("window.__TAURI__.event");
    c.that(
        matches!((mount, wiring), (Some(m), Some(w)) if m < w),
        format!("the character mounts before the side wiring: {mount:?} then {wiring:?}"),
    );
    if let Some(at) = wiring {
        let before = &PAGE[..at];
        c.that(
            before.rfind("try {").map_or(false, |t| t > before.rfind('}').unwrap_or(0)),
            "the side wiring sits inside a try",
        );
    }

    // Bottom-aligning a scrolling column with `justify-content` sends its overflow out of the top,
    // where no scrollbar reaches. Measured: a fourth card landed at y 122 against a container
    // starting at 155, invisible, unreachable, and still reported as a place to click. Matching the
    // declaration and not the two words apart is what lets the page explain itself in a comment.
    c.that(
        !PAGE.contains("justify-content: flex-end"),
        "no scrolling column is bottom-aligned with justify-content",
    );
}

/// `chat.js` is the one file that draws the dialogue (Docs/DESIGN_Pet.md 4.4). That it exists and
/// is about the dialogue is what keeps the lints above from passing by being about an empty file,
/// and the three surface values are the state machine the design chose over two booleans.
fn chat_js_is_the_dialogue_box(c: &mut Checks) {
    c.that(CHAT.len() > 2000, format!("chat.js has content: {} bytes", CHAT.len()));
    c.that(CHAT.contains("window.Chat = "), "chat.js is the file that defines Chat");
    for surface in ["\"hidden\"", "\"bubble\"", "\"log\""] {
        c.that(CHAT.contains(surface), format!("chat.js names the surface {surface}"));
    }
    for member in ["mount:", "show:", "append:", "surface:", "setSurface:", "regions:", "onChoose:"] {
        c.that(CHAT.contains(member), format!("Chat exports {member}"));
    }
    c.that(CHAT.contains("n.textContent = text"), "chat.js sets text with textContent");
    c.eq(innerhtml_assignments(CHAT), Vec::<usize>::new(), "chat.js never assigns to innerHTML");
}

/// The page calls Rust by a command name and argument names. A mismatch on either side arrives as
/// an `Err` the page only logs, and the symptom is a dialogue that draws and cannot be clicked, which
/// looks like a hit-test bug. The same class of silent failure as the last case of `pet_asking`.
fn the_page_and_rust_agree_on_set_regions(c: &mut Checks) {
    let call: String =
        PAGE.find("invoke(\"set_regions\"").map(|at| PAGE[at..].chars().take(300).collect()).unwrap_or_default();
    c.that(!call.is_empty(), "the page calls set_regions");
    c.that(call.contains("character:"), "the page sends a character list");
    c.that(call.contains("panel: panelRegions()"), "and a panel list");
    // One builder, which the dialogue's own regions are the start of. The report prints the same
    // call, so what a headless run shows is what Rust was handed.
    c.that(
        PAGE.contains("return Chat.regions().concat(mine);"),
        "and that list starts from the dialogue's regions",
    );
    c.that(!PAGE.contains("set_hit_rect"), "the page no longer reports a single rectangle");

    c.that(MAIN.contains("fn set_regions("), "Rust defines set_regions");
    c.that(
        MAIN.contains("character: Vec<hit::Region>") && MAIN.contains("panel: Vec<hit::Region>"),
        "and reads the two keys the page sends",
    );
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    c.that(registered.contains("set_regions"), "and registers it as a command");
    c.that(!MAIN.contains("set_hit_rect"), "Rust no longer has the single-rectangle command");
}

/// The declarations of the CSS rule whose selector is exactly `selector` and starts its line. The
/// start-of-line condition is there because `#chatBubble {` is also a substring of any longer
/// selector that ends the same way, and of a comment that quotes it.
fn css_rule<'a>(css: &'a str, selector: &str) -> Option<&'a str> {
    let head = format!("{selector} {{");
    css.match_indices(&head).find_map(|(at, _)| {
        let starts_its_line = css[..at].rsplit('\n').next().is_some_and(|before| before.trim().is_empty());
        let body = &css[at + head.len()..];
        starts_its_line.then(|| body.find('}').map(|end| &body[..end])).flatten()
    })
}

/// A declaration's number, in px, from a rule's body. One declaration per line, which is this
/// file's style, and the whole name has to match: `find("width:")` is also inside
/// `scrollbar-width:`, and a check that reads the wrong number is worse than no check.
fn px_of(body: &str, name: &str) -> Option<f64> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(name)?.strip_prefix(':')?;
        rest.trim().strip_suffix(';')?.strip_suffix("px")?.trim().parse().ok()
    })
}

/// A declaration's base number from `calc(<n>px * var(--ui))`, the form every size that follows the
/// interface scale takes in this stylesheet. The scale multiplies it, so this is its size at 1.
fn scaled_px_of(body: &str, name: &str) -> Option<f64> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(name)?.strip_prefix(':')?.trim();
        rest.strip_prefix("calc(")?.strip_suffix("* var(--ui));")?.trim().strip_suffix("px")?.parse().ok()
    })
}

/// The figure's topmost and lowest opaque points from the 24-frame sampling in Docs/DESIGN_Pet.md
/// 4.4.1, and the `bottom` of `#char` they were sampled at. The figure is drawn the same way at any
/// other `bottom`, so there these hold shifted by the difference.
const FIGURE_TOP_AT_46: f64 = 280.4;
const FIGURE_FEET_AT_46: f64 = 566.8;
const FIGURE_SAMPLED_AT: f64 = 46.0;

/// The visual novel's layout (使用者 2026-11-08, Docs/DESIGN_Pet.md 4.4.6), as arithmetic on the
/// declarations that place each box and not as a string match on them: a box that grew over the
/// field would still contain every word a string match could look for. Read from the bottom of the
/// 460x620 window up, the claims are the ones the layout was chosen for:
///
/// - the text field is in the window and the box ends above it, so the box never covers the microphone;
/// - her feet end inside the box and the box starts below her middle, so it covers her legs and
///   nothing of her shows in the gap between the box and the field;
/// - the strip of buttons ends above the box, and at the largest `--ui` the settings allow it still
///   ends below where the history and the fleet list stop, so neither sheet covers a button;
/// - the choices end above her head, so three cards and her face are both visible.
///
/// This replaced `the_dialogue_sits_in_the_band_above_the_figure`, which held the bubble above her
/// head. What that case protected, that the dialogue keeps its measured distance from her
/// silhouette, is the same kind of claim here, made about the new places.
fn the_dialogue_is_laid_out_like_a_visual_novel(c: &mut Checks) {
    let window = WINDOW_SIZE.1;
    let rule = |selector: &str| css_rule(PAGE, selector).unwrap_or_default();
    let num = |selector: &str, name: &str| px_of(rule(selector), name);

    let Some(char_bottom) = num("#char", "bottom") else {
        return c.that(false, "#char says how far up it sits");
    };
    let shift = char_bottom - FIGURE_SAMPLED_AT;
    let (top, feet) = (FIGURE_TOP_AT_46 - shift, FIGURE_FEET_AT_46 - shift);
    // `--figure` scales her about the bottom of her box (index.html `#char`), so at a figure scale
    // `f` each of her points is `f` times as far above that edge as it is at 1.
    let edge = window - char_bottom;
    let at = |y: f64, f: f64| edge - (edge - y) * f;
    c.that(
        rule("#char").contains("width: calc(320px * var(--figure));")
            && rule("#char").contains("height: calc(320px * var(--figure));")
            && rule("#char").contains("left: calc(230px - 160px * var(--figure));"),
        "her box grows with --figure about the window's middle",
    );
    c.that(
        CHARACTER.contains("const TOP = 33 / 320;") && CHARACTER.contains("h * (1 - TOP - BOTTOM)") && CHARACTER.contains(", h * TOP)"),
        "and character.js fits her to the box in proportion, head room included",
    );
    c.that(
        320.0 * settings::FIGURE.high <= WINDOW_SIZE.0,
        format!("at --figure {} her box is {} wide and fits the window", settings::FIGURE.high, 320.0 * settings::FIGURE.high),
    );

    let placed = |selector: &str| match (num(selector, "bottom"), num(selector, "height")) {
        (Some(b), Some(h)) => Some((window - b - h, window - b)),
        _ => None,
    };
    let (Some(field), Some(boxed)) = (placed("#chatInput"), placed("#chatBubble")) else {
        return c.that(false, "the field and the box each say a bottom and a height");
    };
    c.that(field.0 >= 0.0 && field.1 <= window, format!("the field is inside the window: {field:?}"));
    c.that(boxed.1 <= field.0, format!("the box ends at {} and the field starts at {}", boxed.1, field.0));
    for f in [settings::FIGURE.low, 1.0, settings::FIGURE.high] {
        let (top, feet) = (at(top, f), at(feet, f));
        c.that(
            feet >= boxed.0 && feet <= boxed.1,
            format!("at --figure {f} her feet at {feet} end inside the box, {} to {}", boxed.0, boxed.1),
        );
        c.that(
            boxed.0 >= (top + feet) / 2.0,
            format!("at --figure {f} the box starts at {} and her middle is at {}", boxed.0, (top + feet) / 2.0),
        );
    }

    // The strip: the fleet list's button at one end and the controls at the other, on one line.
    let strip = num("#controls", "bottom");
    c.eq(num("#gate", "bottom"), strip, "the fleet list's button and the controls share the strip");
    let tall = |selector: &str| scaled_px_of(rule(selector), "height");
    c.eq(tall("#gate"), tall("#controls button"), "and are one height");
    match (strip, tall("#controls button"), num("#chatLog", "bottom"), num("#panel", "bottom")) {
        (Some(strip), Some(button), Some(log), Some(panel)) => {
            c.that(window - strip <= boxed.0, format!("the strip ends at {} and the box starts at {}", window - strip, boxed.0));
            let highest = window - strip - button * settings::UI.high;
            for (name, bottom) in [("the history", log), ("the fleet list", panel)] {
                c.that(
                    window - bottom <= highest,
                    format!("{name} stops at {} and the strip reaches {highest} at --ui {}", window - bottom, settings::UI.high),
                );
            }
        }
        got => c.that(false, format!("the strip and the sheets say where they sit, got {got:?}")),
    }
    // A sheet that grew past the top of the window would be clipped by it and still be reported.
    for (root, sheet) in [("#chatLog", ".chat-log"), ("#panel", "#panel")] {
        match (num(root, "bottom"), num(sheet, "max-height")) {
            (Some(b), Some(h)) => c.that(window - b - h >= 0.0, format!("{sheet} reaches up to {} at most", window - b - h)),
            got => c.that(false, format!("{sheet} says how tall it may grow, got {got:?}")),
        }
    }

    // The band's height is `calc(<base>px - <per>px * (var(--figure) - 1))`: it follows her head.
    let band = rule("#chatChoices").lines().find_map(|line| {
        let rest = line.trim().strip_prefix("height: calc(")?.strip_suffix("px * (var(--figure) - 1));")?;
        let (base, per) = rest.split_once("px - ")?;
        Some((base.trim().parse::<f64>().ok()?, per.trim().parse::<f64>().ok()?))
    });
    // One card at the largest text: its padding, border and line, and the margin above it.
    let card = rule(".chat-choice");
    let pad = card.lines().find_map(|l| l.trim().strip_prefix("padding: ")?.split("px").next()?.parse::<f64>().ok());
    let card_h = match (pad, px_of(card, "margin-top"), scaled_px_of(card, "font-size")) {
        (Some(pad), Some(margin), Some(font)) => Some(2.0 * pad + 2.0 + font * settings::UI.high * 1.45 + margin),
        _ => None,
    };
    match (num("#chatChoices", "top"), band, card_h) {
        (Some(t), Some((base, per)), Some(card_h)) => {
            for f in [settings::FIGURE.low, 1.0, settings::FIGURE.high] {
                let (end, head) = (t + base - per * (f - 1.0), at(top, f));
                c.that(end <= head, format!("at --figure {f} the choices end at {end} and her head starts at {head}"));
                c.that(
                    end - t >= 3.0 * card_h,
                    format!("at --figure {f} the band holds three one-line cards at --ui {}: {} for {}", settings::UI.high, end - t, 3.0 * card_h),
                );
            }
        }
        got => c.that(false, format!("#chatChoices says a top, a height that follows --figure, and the cards a size, got {got:?}")),
    }

    // The side went with an older layout. It existed so a window at the screen's edge would not
    // squeeze the bubble between the figure and the edge (Docs/DESIGN_Pet.md 4.4.3, the 26-year
    // 伺か precedent); a box as wide as the window cannot be squeezed that way, so there is
    // nothing left for it to decide and the mechanism is gone rather than inert.
    c.that(css_rule(PAGE, "#chatBubble.left").is_none(), "the bubble has no second side");
    c.that(!PAGE.contains("bubble-side"), "the page does not listen for a side");
    c.that(!MAIN.contains("get_bubble_side"), "Rust has no command for a side");
}

/// Every button in the page's own markup is in the set of regions Rust is sent. A button that is
/// drawn and not reported is one the window lets the click through to the desktop under, which
/// looks like a button that does nothing. The dialogue's own buttons are reported by
/// `Chat.regions`, one rectangle per child of each root; these are the page's.
fn every_button_on_the_page_is_reported(c: &mut Checks) {
    let markup = PAGE.split("<script").next().unwrap_or("");
    let ids: Vec<&str> =
        markup.match_indices("<button id=\"").filter_map(|(at, head)| markup[at + head.len()..].split('"').next()).collect();
    c.that(ids.len() >= 4, format!("the page's buttons: {ids:?}"));
    let regions: String =
        PAGE.find("function panelRegions()").map(|at| PAGE[at..].split("\n}").next().unwrap_or("").to_string()).unwrap_or_default();
    for id in ids {
        let wanted = format!("document.getElementById(\"{id}\");");
        let element = PAGE.lines().find_map(|line| {
            let (name, value) = line.trim().strip_prefix("const ")?.split_once(" = ")?;
            (value == wanted).then_some(name)
        });
        match element {
            Some(name) => c.that(regions.contains(&format!("regionOf({name})")), format!("{id} ({name}) is reported")),
            None => c.that(false, format!("the script holds {id} by a name of its own")),
        }
    }
    c.that(
        CHAT.contains("[roots.bubble, roots.choices, roots.input, roots.log].forEach"),
        "and the dialogue reports the children of all four of its roots",
    );
}

/// The box's "›" goes on to the next piece of her answer when one has come in, and opens or closes
/// the history when none is waiting (使用者 2026-11-08). One control, so the order is the behavior:
/// a waiting page must be offered before the history, or the rest of her answer could only be read
/// there. A piece that arrives becomes a page and the end of her turn in the log, both.
fn the_arrow_pages_through_an_answer_and_then_opens_the_history(c: &mut Checks) {
    let chat = CHAT.replace("\r\n", "\n");
    let body = |head: &str| chat.find(head).and_then(|at| chat[at..].split_once("\n  }").map(|(b, _)| b.to_string())).unwrap_or_default();
    let advance = body("function advance(");
    let page = advance.find("current.page += 1;");
    let history = advance.find("setSurface(surface === Surface.Log ? Surface.Bubble : Surface.Log);");
    c.that(advance.contains("if (waiting()) {"), "the arrow asks whether a page is waiting");
    c.that(
        matches!((page, history), (Some(p), Some(h)) if p < h),
        format!("and turns the page before it would open the history: {page:?} {history:?}"),
    );
    c.that(
        body("function waiting(").contains("current !== null && current.page < current.parts.length - 1"),
        "a page is waiting when the box is on any but the last piece",
    );
    let append = body("function append(");
    c.that(
        append.contains("current.turn.text = before + separator(before) + text;") && append.contains("current.parts.push(text);"),
        "a piece goes on the end of her turn for the log and becomes the next page of the box",
    );
    c.that(chat.contains("parts: [turn.text], page: 0"), "a new answer opens on its first page");
    let box_node = body("function boxNode(");
    c.that(box_node.contains("next.addEventListener(\"click\", advance);"), "the arrow is in the box and runs advance");
    c.that(box_node.contains("names.character"), "and the box carries her name");
    c.that(
        css_rule(PAGE, ".chat-next").is_some_and(|b| b.contains("position: absolute;")) && css_rule(PAGE, ".chat-next.waiting").is_some(),
        "the arrow has a place in the box and a look of its own while a page waits",
    );
}

/// Her voice turns the pages, as a visual novel's auto mode does: a later part's page is shown when
/// that part begins to sound, not when it arrives, only forward, and never for a turn that was
/// stopped or replaced before she got there. With her voice off nothing sounds, so nothing turns.
fn her_voice_turns_the_page(c: &mut Checks) {
    let chat = CHAT.replace("\r\n", "\n");
    let body = |text: &str, head: &str| text.find(head).and_then(|at| text[at..].split_once("\n  }").map(|(b, _)| b.to_string())).unwrap_or_default();
    let turn_to = body(&chat, "function turnTo(");
    c.that(turn_to.contains("page <= current.page"), "turnTo goes only forward, so a page the user turned to stays");
    c.that(turn_to.contains("page >= current.parts.length"), "and only to a page that is there");
    c.that(turn_to.contains("current.answered"), "and not on a turn the user has answered");
    c.that(chat.contains("turnTo: turnTo,") && chat.contains("pages: function ()"), "chat.js exposes turnTo and the page count");

    let page = PAGE.replace("\r\n", "\n");
    let start = body(&page, "async function start(f, mine, actions, joins, onStart)");
    let at = start.find("src.start(at);");
    let timer = start.find("if (playing === turn) onStart();");
    c.that(
        matches!((at, timer), (Some(a), Some(t)) if a < t) && start.contains("(at - context().currentTime) * 1000"),
        "the player calls onStart when the clip begins, and only while its turn is the one playing",
    );
    let queue = body(&page, "function queue(voice, plan, onStart)");
    let muted = queue.find("if (!voiced) return Promise.resolve(false);");
    let passes = queue.find("start(voice, ticket, actionsOf(plan), true, onStart)");
    c.that(
        matches!((muted, passes), (Some(m), Some(p)) if m < p),
        "a later part hands its onStart to the player, and a muted voice returns before it",
    );
    c.that(
        page.contains("Speaker.queue(voice, p.plan, () => { if (page !== null) Chat.turnTo(page); })"),
        "the part's page is turned to when it sounds",
    );
    c.that(
        page.contains("heard.shown = Chat.append(text);\n      if (heard.shown) page = Chat.pages() - 1;"),
        "and that page is the one Chat.append just added",
    );
}

/// The mark by her name and the grain on the paper are files of the theme in effect
/// (Docs/DESIGN_Themes.md), so they can be replaced without editing the page; the page reads them
/// through custom properties and names no picture of its own past the bundled default. The marks
/// are original drawings: the repository is to be published, and the asterisk on the reference
/// picture is another company's logo.
fn the_icon_and_the_paper_are_files_of_their_own(c: &mut Checks) {
    for (property, file, css) in [("--icon", "icon.svg", ".chat-icon"), ("--paper-texture", "paper.svg", ".chat-box")] {
        let on_disk = ui_dir().join("themes").join("paper").join(file);
        let text = std::fs::read_to_string(&on_disk).unwrap_or_default();
        c.that(text.contains("<svg"), format!("ui/themes/paper/{file} is an SVG: {}", on_disk.display()));
        c.that(
            css_rule(PAGE, ":root").is_some_and(|b| b.contains(&format!("{property}: url(\"themes/paper/{file}\");"))),
            format!("the page starts {property} at the bundled 紙本's {file}"),
        );
        c.that(css_rule(PAGE, css).is_some_and(|b| b.contains(&format!("var({property})"))), format!("{css} reads {property}"));
    }
    c.that(!ui_dir().join("theme").exists(), "the old ui/theme folder is gone, its two files moved into themes/paper");
    c.that(!PAGE.contains("\"theme/"), "and the page names nothing in it");
    for theme in ["paper", "night"] {
        let shipped: Vec<String> = std::fs::read_dir(ui_dir().join("themes").join(theme))
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        c.that(
            !shipped.is_empty() && !shipped.iter().any(|f| f.ends_with(".png")),
            format!("{theme} ships no picture taken from the references: {shipped:?}"),
        );
    }
}

/// The fleet list is collapsed behind one button, and the button carries the count.
///
/// The count is the premise of this layout, not a decoration on it. Gate condition 2 measures
/// whether the gatekeeper shortens the time a waiting session waits, and a list nobody is told to
/// open shortens nothing: D2 paid for that lesson once already, which is where "take me there"
/// came from. So a headless run has to be able to see the count without a screenshot.
fn the_fleet_list_is_behind_a_button_that_counts(c: &mut Checks) {
    for marker in ["id=\"gate\"", "id=\"gateN\""] {
        c.that(PAGE.contains(marker), format!("the page has {marker}"));
    }
    c.that(
        css_rule(PAGE, "#panel.shut").is_some_and(|body| body.contains("display: none")),
        "a shut list is not drawn at all",
    );
    c.that(css_rule(PAGE, "#gate.wait").is_some(), "the button has a rule for something waiting");
    // The open list is the one thing meant to overlap the character, and it sits before `#char` in
    // the document, so it needs to say it paints above. Measured by looking at it: every rectangle
    // was right and the session names were behind an arm. The pair is the check, because a z-index
    // on the figure would undo the one on the list without touching it.
    c.that(
        css_rule(PAGE, "#panel").is_some_and(|body| body.contains("z-index:")),
        "the open list says it paints above the figure",
    );
    c.that(
        css_rule(PAGE, "#char").is_some_and(|body| !body.contains("z-index")),
        "and the figure does not claim a layer of its own",
    );
    c.that(
        PAGE.contains("gateNEl.textContent = fleetErr ? \"?\" : String(waiting)"),
        "the number on the button is the waiting count, and a failed look is not zero",
    );

    // Drawn and pressable are two different things here. The button is always reported; the list
    // only while it is open, because a shut one is `display: none`, and a zero-area rectangle makes
    // Rust reject the whole report and freeze every region at the last good set.
    let regions: String =
        PAGE.find("function panelRegions()").map(|at| PAGE[at..].chars().take(400).collect()).unwrap_or_default();
    c.that(regions.contains("regionOf(gateEl)"), "the button is always in the reported set");
    c.that(
        regions.contains("if (!panelEl.classList.contains(\"shut\")) mine.push(regionOf(panelEl))"),
        "and the list is in it only while it is open",
    );
    c.that(PAGE.contains("panel: panelRegions()"), "set_regions sends that one list");
    c.that(PAGE.contains("panelFrac: panelRegions()"), "and the report prints the same one, not a second build of it");
}

/// The conversation core's source, and the two files that decide what the webview may do. Embedded
/// for the reason `PAGE` is: this is the text that was compiled, not a copy that could drift.
const CORE: &str = include_str!("core.rs");
const CAPABILITIES: &str = include_str!("../capabilities/default.json");
const CONF: &str = include_str!("../tauri.conf.json");
const MANIFEST: &str = include_str!("../Cargo.toml");

/// What the dialogue says used to come from a fixture in the page. The core is the only source now,
/// so nothing may bring a script back, and a sentence typed or a card pressed has to go to Rust
/// (Docs/DESIGN_Pet.md 6.1).
fn the_fixture_is_gone_and_the_page_asks_the_core(c: &mut Checks) {
    for needle in ["const SCRIPT", "SCRIPT[", "scriptAt"] {
        c.that(!PAGE.contains(needle), format!("the page has no {needle}"));
    }
    // The dialogue is still mounted, so the absences above are not those of an emptied page.
    c.that(PAGE.contains("Chat.mount(chatEls"), "the page still mounts the dialogue");

    let handler: String =
        PAGE.find("Chat.onChoose(").map(|at| PAGE[at..].chars().take(160).collect()).unwrap_or_default();
    c.that(!handler.is_empty(), "the page handles a typed sentence or a pressed card");
    c.that(handler.contains("invoke(\"say\", { text })"), "and hands that sentence to the core through say");
    c.that(PAGE.contains("invoke(\"core_state\")"), "the page asks once what the core is doing");
}

/// The page and Rust meet in names and in strings, and a mismatch on either side arrives as nothing:
/// no reply, no error. The same silent class as `set_regions` and the bubble side above.
fn the_page_and_rust_agree_on_the_core(c: &mut Checks) {
    c.that(PAGE.contains(&format!("listen(\"{CORE_EVENT}\"")), "the page listens for what the core printed");
    c.that(PAGE.contains(&format!("listen(\"{CORE_STATE_EVENT}\"")), "and for what became of the core");

    // The page branches on the word Rust uses for Down and reads the reason under the key Rust
    // writes it to. Both come from the serialised value, not from a copy of them in this test.
    let down = serde_json::to_value(CoreState::Down { why: "x".to_string() }).unwrap_or(Value::Null);
    let tag = down.get("state").and_then(Value::as_str).unwrap_or("?");
    c.that(
        PAGE.contains(&format!("s.state === \"{tag}\"")),
        format!("the page branches on {tag:?}, which is how Rust spells Down"),
    );
    let reason = down.as_object().and_then(|o| o.keys().find(|k| *k != "state").cloned()).unwrap_or_default();
    c.that(PAGE.contains(&format!("s.{reason}")), format!("and reads why under {reason:?}"));

    // The core's own words, restated in `pet_core` as literal lines: the page reads these keys.
    for read in ["p.ev === \"utterance\"", "p.ev === \"error\"", "p.text", "p.part", "p.message", "p.voice", "voice.state === \"voiced\"", "voice.why"] {
        c.that(PAGE.contains(read), format!("the page reads {read}"));
    }

    c.that(MAIN.contains("fn core_state("), "Rust defines core_state");
    c.that(MAIN.contains("fn say("), "Rust defines say");
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    for command in ["core_state", "say"] {
        let listed = registered.split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|name| name == command);
        c.that(listed, format!("and registers {command} as a command"));
    }
    c.that(MAIN.contains(".manage(core::Core::default())"), "Rust manages the core's state");
    c.that(MAIN.contains("core::start("), "Rust starts the core when the app does");
    c.that(
        CORE.contains("app.emit(EVENT") && CORE.contains("app.emit(STATE_EVENT"),
        "and emits the two events under the names the page listens for",
    );
}

/// The core's wiring is optional and the character is not, so it comes after the character and
/// inside a try. A throw at load time runs off the end of the script, and the bubble side wiring
/// already taught this page what that costs. Its own lint, because `the_dialogue_is_in_three_places`
/// follows the first wiring it finds and this one comes after.
fn the_core_wiring_comes_after_the_character_and_is_wrapped(c: &mut Checks) {
    let mount = PAGE.find("Character.mount(");
    let wiring = PAGE.find(&format!("listen(\"{CORE_EVENT}\""));
    c.that(
        matches!((mount, wiring), (Some(m), Some(w)) if m < w),
        format!("the character mounts before the core wiring: {mount:?} then {wiring:?}"),
    );
    if let Some(at) = wiring {
        let before = &PAGE[..at];
        c.that(
            before.rfind("try {").is_some_and(|t| t > before.rfind('}').unwrap_or(0)),
            "the core wiring sits inside a try",
        );
    }
    // Wired badly, the character would answer nothing and say nothing, which is the failure this
    // whole page exists not to have. Both ways the wiring can fail end in the bubble.
    c.eq(PAGE.matches("trouble(\"接不上對話核心").count(), 2, "a wiring that throws or rejects is said in the bubble");
}

/// What would give the webview a way to start a process: the shell plugin's crate, its permission
/// prefix and its API. Each is spelled so that a sentence about the plugin does not match, because
/// the files scanned include their own comments and the rule has to be explainable in one.
fn shell_grants(source: &str) -> Vec<&'static str> {
    ["tauri_plugin_shell", "tauri-plugin-shell", "plugin:shell", "\"shell:", ".shell()"]
        .into_iter()
        .filter(|needle| source.contains(needle))
        .collect()
}

/// The core is started by Rust with `std::process`, so the webview has no way to start anything and
/// no permission exists that could be opened too wide (Docs/DESIGN_Pet.md 6, the 2026-11-03 note).
fn the_webview_has_no_way_to_start_a_process(c: &mut Checks) {
    // The detector has to be able to see what it hunts, or the empty answers below prove nothing.
    for (sample, what) in [
        (r#"{"permissions":["core:default","shell:allow-execute"]}"#, "a shell permission"),
        ("tauri-plugin-shell = \"2\"", "the plugin as a dependency"),
        ("tauri_plugin_shell::init()", "the plugin being registered"),
        ("app.shell().sidecar(\"x\")", "the plugin's API"),
    ] {
        c.that(!shell_grants(sample).is_empty(), format!("the detector sees {what}"));
    }

    for (name, source) in [
        ("capabilities/default.json", CAPABILITIES),
        ("Cargo.toml", MANIFEST),
        ("main.rs", MAIN),
        ("core.rs", CORE),
        ("stt.rs", STT),
    ] {
        c.eq(shell_grants(source), Vec::<&str>::new(), format!("{name} has nothing from the shell plugin"));
    }

    // Both files scanned are the real ones, not empty stand-ins.
    let permissions = serde_json::from_str::<Value>(CAPABILITIES)
        .ok()
        .and_then(|v| v.get("permissions").and_then(Value::as_array).map(|list| list.len()))
        .unwrap_or(0);
    c.that(permissions > 0, format!("the capability file lists permissions: {permissions}"));
    c.that(CORE.contains("Command::new("), "core.rs is what starts the core, and with std::process");
    c.that(STT.contains("Command::new("), "stt.rs starts the speech input service the same way");
}

/// An answer arrives in parts, each an `utterance` with a `part` number. Part 0, or a line with no
/// number, is the old path: it stops what is sounding, opens a new bubble and starts a turn with
/// `Speaker.speak`. A part after it belongs to that turn. Its voice goes behind the turn's sound with
/// `Speaker.queue`, and it must not reach `speak`, which would start a turn of its own and cut the
/// one that is talking, or `stop`, which would end it.
///
/// Pinned as text, like the rest of this suite, because this crate cannot run the page.
fn a_later_part_goes_to_the_queue_and_not_to_speak(c: &mut Checks) {
    let body = page_block("function onCoreEvent(");
    c.that(!body.is_empty(), "the page has the core's event handler");
    c.that(
        body.contains("const later = Number.isInteger(p.part) && p.part >= 1;"),
        "part 1 and up is told from part 0 by its number, and a line with no number is part 0",
    );
    let guard = body.find("if (!later) {");
    let stop = body.find("Speaker.stop();");
    c.that(
        matches!((guard, stop), (Some(g), Some(s)) if g < s),
        format!("only part 0 stops what is sounding: {guard:?} then {stop:?}"),
    );
    c.eq(body.matches("Speaker.stop(").count(), 1, "and that is the one place the handler stops it");
    c.that(
        body.contains("const sounded = later\n        ? Speaker.queue(voice, p.plan, ")
            && body.contains("\n        : Speaker.speak(voice, p.plan);"),
        "a later part's voice is queued behind the turn, and part 0's voice starts it",
    );
    c.eq(body.matches("Speaker.speak(").count(), 1, "and speak is called from nowhere else");
    c.that(
        body.contains("heard.shown = Chat.append(text);")
            && body.contains("Chat.show({ who: Chat.Who.Character, text, choices: [] });"),
        "a later part's words go on the end of the bubble, and part 0 opens a new one",
    );
    // A silent part says why by the rule a silent turn does, from the one test.
    c.eq(
        body.matches("if (!silenceExplained) {").count(),
        1,
        "a silent part follows the once-per-outage rule through the same test as a silent turn",
    );
}

/// The pieces of an answer are joined the way the core joins the clauses it keeps: no space after a
/// Chinese mark, one space after anything else. The marks are the Chinese ones of the planner's own
/// list of punctuation (`cue/align.rs`), so a mark added there is a decision made here too.
fn the_bubble_joins_a_part_the_way_the_core_does(c: &mut Checks) {
    let chat = CHAT.replace("\r\n", "\n");
    let head = "const NO_SPACE_AFTER = \"";
    let marks: String = chat
        .find(head)
        .map(|at| chat[at + head.len()..].chars().take_while(|&ch| ch != '"').collect())
        .unwrap_or_default();
    c.that(!marks.is_empty(), "chat.js names the marks that take no space after them");
    let punctuation = "，。？！、；：,.?!;:…";
    for ch in punctuation.chars() {
        c.that(align::is_punctuation(ch), format!("{ch} is one of the planner's marks"));
        c.eq(marks.contains(ch), !ch.is_ascii(), format!("{ch} is a mark with no space after it exactly when it is Chinese"));
    }
    c.eq(
        marks.chars().count(),
        punctuation.chars().filter(|ch| !ch.is_ascii()).count(),
        "and the set holds those marks and nothing else",
    );

    let separator: &str =
        chat.find("function separator(").and_then(|at| chat[at..].split_once("\n  }").map(|(body, _)| body)).unwrap_or("");
    c.that(separator.contains("if (before === \"\") return \"\";"), "an empty bubble gets no leading space");
    c.that(
        separator.contains("NO_SPACE_AFTER.indexOf(before.charAt(before.length - 1)) >= 0 ? \"\" : \" \""),
        "otherwise the join is one space, unless the bubble already ends on one of those marks",
    );
    c.that(
        chat.contains("current.turn.text = before + separator(before) + text;"),
        "and the new words go after that separator",
    );
}

/// The user's reply marks the turn answered, and a part that comes in after it has nowhere to go: it
/// is refused before anything is changed or drawn, and the page is told.
fn the_bubble_refuses_a_part_for_an_answered_turn(c: &mut Checks) {
    let chat = CHAT.replace("\r\n", "\n");
    let append: &str =
        chat.find("function append(").and_then(|at| chat[at..].split_once("\n  }").map(|(body, _)| body)).unwrap_or("");
    c.that(!append.is_empty(), "chat.js has append");
    let refuse = append.find("if (!current || current.answered) return false;");
    let change = append.find("current.turn.text =");
    let draw = append.find("render();");
    c.that(
        matches!((refuse, change, draw), (Some(r), Some(t), Some(d)) if r < t && r < d),
        format!("append refuses before it changes the text or draws: {refuse:?} {change:?} {draw:?}"),
    );
    c.that(chat.contains("current.answered = true;"), "and it is the user's reply that marks a turn answered");
    c.that(chat.contains("append: append,"), "Chat exports append");
    c.that(PAGE.contains("heard.shown = Chat.append(text);"), "and the page keeps the answer, so the report can say a part was refused");
}

/// The headless report says how many parts the last turn had and how each sounded, and never the
/// audio.
fn the_report_says_how_each_part_of_the_last_turn_sounded(c: &mut Checks) {
    let note = page_block("function noteTurn(");
    c.that(
        note.contains("if (!later || !lastVoice) {") && note.contains("lastVoice = { parts: [] };") && note.contains("lastPlan = [];"),
        "part 0 starts the report's two records over",
    );
    c.that(
        note.contains("lastVoice.parts.push(entry);") && note.contains("lastPlan.push(plan);"),
        "and every part adds its own entry and its own plan, in the order they came",
    );
    let body = page_block("function onCoreEvent(");
    c.eq(body.matches("noteTurn(").count(), 2, "a voiced part and a silent one both go in");
    for field in [
        "part: later ? p.part : 0",
        "state: \"voiced\"",
        "samples: voice.samples",
        "frames: Array.isArray(voice.envelope)",
        "scheduled: null",
        "state: String(voice.state), why",
        "heard.scheduled = ok;",
    ] {
        c.that(body.contains(field), format!("an entry says {field}"));
    }
    c.that(!body.contains("pcm_b64"), "and no entry carries the audio");
    c.that(PAGE.contains("voice: lastVoice,") && PAGE.contains("plan: lastPlan,"), "the report prints both records");
}

// -------------------------------------------------------------------------------------------
// pet_core: the pet's end of the conversation core (Docs/DESIGN_Pet.md 6.1). The lines are written
// out literally, as ChatCore's own self-test does, so this checks the wire and not the struct that
// wrote it. Pure, so it needs neither a process nor a window.

fn an_utterance_line_comes_through_whole(c: &mut Checks) {
    let line = r#"{"ev":"utterance","text":"你好\n\"小毛\"","tags":["wave"],"extra":{"n":1}}"#;
    match parse_line(line.as_bytes()) {
        Err(why) => c.that(false, format!("a well-formed utterance parses: {why}")),
        Ok(event) => {
            c.eq(event.get("ev").and_then(Value::as_str), Some("utterance"), "it is an utterance");
            c.eq(
                event.get("text").and_then(Value::as_str),
                Some("你好\n\"小毛\""),
                "the text is the sentence, unescaped once and otherwise untouched",
            );
            c.eq(event.get("tags"), Some(&json!(["wave"])), "the tags arrive");
            c.eq(
                Some(event),
                serde_json::from_str::<Value>(line).ok(),
                "the payload is the object the core printed, a field this file has never heard of included",
            );
        }
    }
}

fn an_error_line_comes_through(c: &mut Checks) {
    let line = r#"{"ev":"error","message":"bad request: expected value at line 1 column 1"}"#;
    match parse_line(line.as_bytes()) {
        Err(why) => c.that(false, format!("an error event parses: {why}")),
        Ok(event) => {
            c.eq(event.get("ev").and_then(Value::as_str), Some("error"), "it is an error");
            c.eq(
                event.get("message").and_then(Value::as_str),
                Some("bad request: expected value at line 1 column 1"),
                "the message is the core's, word for word",
            );
        }
    }
}

fn every_other_event_is_forwarded_too(c: &mut Checks) {
    // `viseme` is an event nobody has defined. It is forwarded, because the page decides what an
    // event means and a core that grows a new one must not be silenced by this file.
    for (line, name) in [
        (r#"{"ev":"ready","version":"0.1.0"}"#, "ready"),
        (r#"{"ev":"turn_end"}"#, "turn_end"),
        (r#"{"ev":"viseme","at":0.4}"#, "viseme"),
    ] {
        let got = parse_line(line.as_bytes()).ok().and_then(|e| e.get("ev").and_then(Value::as_str).map(str::to_string));
        c.eq(got, Some(name.to_string()), format!("{name} is forwarded"));
    }
}

fn a_line_that_is_not_an_event_is_not_the_end_of_the_loop(c: &mut Checks) {
    let lines: [&[u8]; 9] = [
        br#"{"ev":"ready","version":"0.1.0"}"#,
        br#"{"ev":"utterance","text":"cut off"#,
        b"\xff\xfe not utf-8",
        br#"["ev","utterance"]"#,
        br#"{"text":"no ev"}"#,
        br#"{"ev":5}"#,
        b"   ",
        br#"{"ev":"utterance","text":"after the noise","tags":[]}"#,
        br#"{"ev":"turn_end"}"#,
    ];
    let mut input = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        input.extend_from_slice(line);
        // The first line ends the way a Windows tool ends one, and the last has no end at all.
        if at == 0 {
            input.extend_from_slice(b"\r\n");
        } else if at + 1 < lines.len() {
            input.push(b'\n');
        }
    }

    let mut seen: Vec<String> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let end = pump(&input[..], |line| match line {
        Ok(event) => seen.push(format!("event {}", event.get("ev").and_then(Value::as_str).unwrap_or("?"))),
        Err(why) => {
            seen.push("bad".to_string());
            reasons.push(why);
        }
    });
    c.that(end.is_ok(), format!("the loop ran to the end of the input: {end:?}"));
    let want: Vec<String> = ["event ready", "bad", "bad", "bad", "bad", "bad", "event utterance", "event turn_end"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    c.eq(seen, want, "each line is accounted for in order, and the loop went on after every bad one");
    c.that(
        reasons.len() == 5 && reasons[0].contains("JSON") && reasons[1].contains("UTF-8"),
        format!("a truncated line and a line of bytes are named for what they are: {reasons:?}"),
    );
    c.that(
        reasons.iter().skip(2).all(|why| why.contains("not an event")),
        format!("so are JSON that is no event: {reasons:?}"),
    );

    // A broken pipe is the one thing that ends the loop with an error, and it is reported.
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the pipe broke"))
        }
    }
    let mut delivered = 0;
    let end = pump(std::io::BufReader::new(Broken), |_| delivered += 1);
    c.that(end.is_err() && delivered == 0, format!("a broken pipe ends the loop with an error: {end:?}"));
}

fn a_bad_line_reaches_the_page_as_an_error_it_already_handles(c: &mut Checks) {
    let event = bad_line_event("it is not JSON: x");
    c.eq(event.get("ev").and_then(Value::as_str), Some("error"), "it is said as an error event");
    let message = event.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
    c.that(message.contains("it is not JSON: x"), format!("the reason is in the message: {message:?}"));
    c.that(
        parse_line(event.to_string().as_bytes()).is_ok(),
        "and it passes the test every line of the core's passes",
    );
}

fn the_say_line_is_one_json_line_carrying_the_text_exactly(c: &mut Checks) {
    c.eq(say_line("hi"), "{\"op\":\"say\",\"text\":\"hi\"}\n".to_string(), "the line the core reads, spelled out");

    // Each of these has broken some line-oriented protocol before: a newline ends the frame early,
    // a quote ends the string, and the last one is a sentence that looks like a different request.
    for text in [
        "plain",
        "",
        "兩行\n第二行",
        "she said \"no\"",
        "back\\slash",
        "tab\there",
        "crlf\r\nline",
        "ctrl\u{1}\u{1f}",
        "😀 and \u{2028} a separator",
        "}{ not framing",
        "{\"op\":\"shutdown\"}",
    ] {
        let line = say_line(text);
        let body = line.strip_suffix('\n').unwrap_or("");
        c.that(
            !body.is_empty() && !body.contains('\n') && !body.contains('\r'),
            format!("{text:?} goes out as exactly one line"),
        );
        match serde_json::from_str::<Value>(body) {
            Err(e) => c.that(false, format!("{text:?} goes out as JSON: {e}")),
            Ok(sent) => {
                c.eq(sent.get("op").and_then(Value::as_str), Some("say"), format!("{text:?} is a say"));
                c.eq(sent.get("text").and_then(Value::as_str), Some(text), format!("{text:?} arrives as it left"));
                c.eq(sent.as_object().map(|o| o.len()), Some(2), format!("and nothing else rides along with {text:?}"));
            }
        }
    }
}

fn the_state_follows_the_core_and_down_is_final(c: &mut Checks) {
    let ready = json!({ "ev": "ready", "version": "0.1.0" });
    let up = CoreState::Ready { version: "0.1.0".to_string() };
    let down = CoreState::Down { why: "gone".to_string() };

    c.eq(CoreState::default(), CoreState::Starting, "a core is starting until it says otherwise");
    c.eq(CoreState::Starting.after(&ready), Some(up.clone()), "ready moves starting to ready, with the version");
    for other in [
        json!({ "ev": "utterance", "text": "x", "tags": [] }),
        json!({ "ev": "turn_end" }),
        json!({ "ev": "error", "message": "m" }),
        json!({ "ev": "readyish" }),
        json!({ "no": "ev" }),
    ] {
        c.eq(CoreState::Starting.after(&other), None, format!("{other} changes nothing"));
    }
    c.eq(up.after(&ready), None, "the same ready again is not news");
    c.eq(
        up.after(&json!({ "ev": "ready", "version": "0.2.0" })),
        Some(CoreState::Ready { version: "0.2.0".to_string() }),
        "a different version is",
    );
    c.eq(down.after(&ready), None, "a core that is down does not come back because a line says so");
    c.eq(
        CoreState::Starting.after(&json!({ "ev": "ready" })),
        Some(CoreState::Ready { version: "?".to_string() }),
        "a ready with no version is still ready",
    );
}

fn the_state_reaches_the_page_under_the_names_it_reads(c: &mut Checks) {
    c.eq(serde_json::to_value(CoreState::Starting).ok(), Some(json!({ "state": "starting" })), "starting");
    c.eq(
        serde_json::to_value(CoreState::Ready { version: "0.1.0".to_string() }).ok(),
        Some(json!({ "state": "ready", "version": "0.1.0" })),
        "ready carries the version",
    );
    c.eq(
        serde_json::to_value(CoreState::Down { why: "gone".to_string() }).ok(),
        Some(json!({ "state": "down", "why": "gone" })),
        "down carries the reason",
    );
    c.eq(CORE_EVENT, "core-event", "the event that carries the core's lines");
    c.eq(CORE_STATE_EVENT, "core-state", "the event that carries its state");
}

fn the_home_the_hook_writes_is_the_one_the_pet_reads(c: &mut Checks) {
    use crate::home::resolve;
    let profile = Some(OsString::from(r"C:\Users\someone"));
    c.eq(resolve(None, profile.clone()), Ok(PathBuf::from(r"C:\Users\someone\.vrpet")), "unset is .vrpet in the profile");
    c.eq(resolve(Some(OsString::from(r"X:\pet")), profile.clone()), Ok(PathBuf::from(r"X:\pet")), "VRPET_HOME wins");
    c.eq(resolve(Some(OsString::new()), profile), Ok(PathBuf::from(r"C:\Users\someone\.vrpet")), "an empty variable is no variable");
    c.that(resolve(None, None).is_err(), "no profile and no variable is an error, not a relative path");

    // MSIX redirects AppData writes made under the Claude desktop app, so a hook writing there and
    // a pet reading there see two folders. Nothing that crosses between them may name it.
    const HOOK: &str = include_str!("bin/vrpet-hook.rs");
    c.that(HOOK.contains("#[path = \"../home.rs\"]"), "the hook takes its directory from the same file as the pet");
    for (name, src) in [("main.rs", MAIN), ("vrpet-hook.rs", HOOK)] {
        c.that(!src.contains("var_os(\"LOCALAPPDATA\")"), format!("{name} does not read LOCALAPPDATA"));
    }
}

fn the_core_is_looked_for_where_the_design_says(c: &mut Checks) {
    let exe = format!("vrchatcore{}", std::env::consts::EXE_SUFFIX);
    let root = Path::new(r"R:\repo");
    let dir = root.join("Pet").join("src-tauri").join("target").join("debug");
    let built = |profile: &str| root.join("ChatCore").join("target").join(profile).join(&exe);

    c.eq(
        candidates(None, Some(&dir), false),
        vec![dir.join(&exe), built("debug"), built("release")],
        "a debug pet looks beside itself, then for the debug core, then the release one",
    );
    c.eq(
        candidates(None, Some(&dir), true),
        vec![dir.join(&exe), built("release"), built("debug")],
        "a release pet prefers the release core",
    );
    let mine = OsString::from(r"X:\mine\core.exe");
    c.eq(
        candidates(Some(mine.clone()), Some(&dir), false),
        vec![PathBuf::from(&mine)],
        "a path named outright is the only place looked, so a wrong one fails and does not run another build",
    );
    c.eq(
        candidates(Some(OsString::new()), Some(&dir), false),
        candidates(None, Some(&dir), false),
        "an empty variable is no variable",
    );
    let shallow = Path::new(r"R:\");
    c.eq(
        candidates(None, Some(shallow), false),
        vec![shallow.join(&exe)],
        "a pet with no repository above it looks only beside itself",
    );
    c.eq(candidates(None, None, false), Vec::<PathBuf>::new(), "with no directory and no name there is nowhere to look");
}

fn the_search_says_what_it_tried(c: &mut Checks) {
    let first = PathBuf::from(r"R:\a\vrchatcore.exe");
    let second = PathBuf::from(r"R:\b\vrchatcore.exe");
    let third = PathBuf::from(r"R:\c\vrchatcore.exe");
    let tried = [first.clone(), second.clone(), third.clone()];

    c.eq(locate(&tried, |p| p == second.as_path()), Ok(second.clone()), "the one that exists is found");
    c.eq(
        locate(&tried, |p| p == first.as_path() || p == third.as_path()),
        Ok(first.clone()),
        "when two exist the better placed wins",
    );
    let why = locate(&tried, |_| false).err().unwrap_or_default();
    for place in &tried {
        c.that(why.contains(&place.display().to_string()), format!("the failure names {}: {why}", place.display()));
    }
    c.that(why.contains(EXE_ENV), format!("and says how to point at the core: {why}"));
    c.that(locate(&[], |_| true).is_err(), "with nowhere to look it fails and does not claim a find");
}

fn an_exit_says_its_code(c: &mut Checks) {
    c.that(exit_why(Some(3)).contains('3'), format!("the code is in it: {}", exit_why(Some(3))));
    c.that(!exit_why(None).is_empty(), "and an exit with no code still says something");
}

// ---------------------------------------------------------------------------------------------
// pet_filler: the pre-rendered thinking bank, and the rule that picks from it.
//
// These assert values, not that the bank loaded. That distinction is the lesson `chat_gate` and
// `pet_render` both cost this project: a first version of each asserted a quantity that was
// identically zero and passed anyway (Docs/DESIGN_Pet.md:487, :1156). So every case below either
// compares against a number the packer computed independently, or refuses a bank of silence.

fn the_bank_is_present_and_every_filler_is_short(c: &mut Checks) {
    let bank = voice::bank();
    c.that(!bank.is_empty(), "the bank holds at least one filler");
    for f in bank {
        // A filler exists to cover the wait, so one that outlasts it makes the wait longer. The
        // renderer's own ceiling is 2.5 s of speech (Scripts/tts_engines/fillers.py).
        c.that(f.secs > 0.2 && f.secs <= 2.5, format!("{} is {:.2} s, outside 0.2 to 2.5", f.name, f.secs));
        c.that(!f.pcm_b64.is_empty(), format!("{} carries audio", f.name));
        c.that(!f.text.is_empty(), format!("{} says what it is", f.name));
        c.eq(f.samples, f.envelope.len() * voice::FRAME_SAMPLES, format!("{}: samples agree with frames", f.name));
    }
}

fn envelope_has_one_frame_per_chunk_and_is_not_all_zero(c: &mut Checks) {
    for f in voice::bank() {
        let loud = f.envelope.iter().filter(|&&v| v > 0.0).count();
        // Not "the envelope exists": an envelope of zeros is a mouth that never opens, and it
        // would pass every shape check while making the feature look broken on screen.
        c.that(loud >= 5, format!("{}: only {loud} frames above the silence gate", f.name));
        c.that(
            f.envelope.iter().all(|&v| (0.0..=1.0).contains(&v)),
            format!("{}: an envelope value is outside 0..1", f.name),
        );
        // The packer normalises by the loudest frame, so exactly one frame must reach 1.0. This is
        // what catches an envelope that was computed against some other clip's peak.
        let peak = f.envelope.iter().copied().fold(0.0_f32, f32::max);
        c.that((peak - 1.0).abs() < 1e-3, format!("{}: loudest frame is {peak:.4}, not 1.0", f.name));
    }
}

fn pcm_is_the_length_the_sample_count_implies(c: &mut Checks) {
    for f in voice::bank() {
        // Checked by arithmetic rather than by decoding, so this crate needs no base64 decoder for
        // a bank it never decodes at run time: the string goes to the page as it is. Two bytes per
        // sample, PCM16.
        c.eq(f.pcm_b64.len(), voice::b64_len(f.samples * 2), format!("{}: base64 length", f.name));
    }
}

fn round_robin_visits_every_filler_before_repeating(c: &mut Checks) {
    let bank = voice::bank();
    let n = bank.len();
    let seen: Vec<&str> = (0..n).map(|t| voice::pick(bank, t).map(|f| f.name.as_str()).unwrap_or("")).collect();
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    c.eq(sorted.len(), n, "the first n turns visit n distinct fillers");
    // And it wraps, rather than running off the end or sticking on the last one.
    c.eq(voice::pick(bank, n).map(|f| f.name.as_str()), Some(seen[0]), "turn n wraps to the first");
    c.eq(voice::pick(bank, n + 1).map(|f| f.name.as_str()), seen.get(1).copied(), "and turn n+1 to the second");
    // No two consecutive turns repeat, which is the one property the rule exists for.
    let repeats = (1..n * 3).filter(|&t| {
        voice::pick(bank, t).map(|f| &f.name) == voice::pick(bank, t - 1).map(|f| &f.name)
    }).count();
    c.eq(repeats, 0, "no filler plays twice in a row over three full rotations");
}

fn an_empty_bank_picks_nothing(c: &mut Checks) {
    // The negative control. `bank()` refuses an empty bank, so this drives `pick` directly: if it
    // ever answered on an empty slice it would be indexing out of bounds.
    let empty: Vec<voice::Filler> = Vec::new();
    c.that(voice::pick(&empty, 0).is_none(), "an empty bank picks nothing at turn 0");
    c.that(voice::pick(&empty, 7).is_none(), "and nothing at turn 7");
}

fn the_page_plays_on_send_and_stops_on_an_answer(c: &mut Checks) {
    // A guard against silent deletion, not a behaviour test: the decode and playback are checked
    // by running the real module against a stub. What this catches is the send path being
    // refactored and the filler quietly stopping happening, which would look exactly like the
    // feature never having been built.
    let send = PAGE.find("Chat.onChoose(");
    let play = PAGE.find("Speaker.filler()");
    let utterance = PAGE.find("p.ev === \"utterance\"");
    let stop = PAGE.find("Speaker.stop()");
    c.that(send.is_some() && play.is_some(), "the page has a send path and a filler play call");
    c.that(utterance.is_some() && stop.is_some(), "and an utterance path and a filler stop call");
    if let (Some(send), Some(play)) = (send, play) {
        c.that(play > send, "the play call is inside the send path, not before it");
    }
    if let (Some(utterance), Some(stop)) = (utterance, stop) {
        c.that(stop > utterance, "the stop call is inside the utterance path");
    }
    // Not awaited and not chained onto `say`: a filler that fails must not stop the sentence.
    c.that(
        !PAGE.contains("await Speaker.filler()") && !PAGE.contains("then(() => Speaker.filler())"),
        "the filler is not chained onto the sentence being sent",
    );

    // The junction the two behaviour checks cannot reach between them. `voice::pick` is proven
    // pure and the page module is proven against a stub, but a command left out of the handler, or
    // a turn counter never given to the app, fails at the user's first click and nowhere earlier.
    c.that(MAIN.contains("filler"), "the filler command exists in main.rs");
    let registered = MAIN
        .split_once("generate_handler!")
        .and_then(|(_, rest)| rest.split_once(']'))
        .is_some_and(|(list, _)| list.contains("filler"));
    c.that(registered, "and it is registered in the invoke handler");
    c.that(
        MAIN.contains(".manage(AtomicUsize::new(0))"),
        "and the turn counter it reads is managed by the app",
    );
}

/// A filler as `render_fillers.py` writes one, `frames` envelope frames long. The audio is not real:
/// nothing in this crate decodes it, and its length is what is checked.
fn bank_filler(name: &str, frames: usize) -> Value {
    let samples = frames * voice::FRAME_SAMPLES;
    json!({
        "name": name, "text": "嗯", "secs": samples as f64 / f64::from(voice::SAMPLE_RATE), "speech_secs": 0.3,
        "samples": samples, "envelope": vec![0.5; frames], "pcm_b64": "A".repeat(voice::b64_len(samples * 2)),
        "heard": "", "cer": -1.0,
    })
}

fn bank_bytes(fillers: Value) -> Vec<u8> {
    let doc = json!({ "about": "test", "sample_rate": 16000, "frame_samples": 320, "voice": "good", "fillers": fillers });
    serde_json::to_vec(&doc).unwrap_or_default()
}

/// A character's bank is someone's file. It is taken in the shape `render_fillers.py` writes, and
/// refused, with the reason, in every way that would make the page throw or the mouth misread.
fn a_bank_file_is_checked_against_how_the_page_plays_it(c: &mut Checks) {
    match voice::parse_bank(&bank_bytes(json!([bank_filler("line_0", 30), bank_filler("line_1", 40)]))) {
        Ok(fillers) => {
            let names: Vec<&str> = fillers.iter().map(|f| f.name.as_str()).collect();
            c.eq(names, vec!["line_0", "line_1"], "a bank as render_fillers.py writes it is taken, in its order");
        }
        Err(why) => c.that(false, format!("a bank as render_fillers.py writes it is taken: {why}")),
    }
    c.eq(
        voice::parse_bank(include_str!("voice/fillers.json").as_bytes()).map(|f| f.len()).ok(),
        Some(voice::bank().len()),
        "and the compiled bank passes the same checks",
    );

    let with = |edit: &dyn Fn(&mut Value)| {
        let mut doc: Value = serde_json::from_slice(&bank_bytes(json!([bank_filler("line_0", 30)]))).unwrap_or(Value::Null);
        edit(&mut doc);
        voice::parse_bank(&serde_json::to_vec(&doc).unwrap_or_default())
    };
    let refused: [(&str, &dyn Fn(&mut Value), &str); 7] = [
        ("a 22 kHz bank", &|d| d["sample_rate"] = json!(22050), "sample_rate"),
        ("a 160-sample frame", &|d| d["frame_samples"] = json!(160), "frame_samples"),
        ("an empty bank", &|d| d["fillers"] = json!([]), "no filler"),
        ("a sample count the envelope does not have", &|d| d["fillers"][0]["samples"] = json!(9601), "envelope frames"),
        ("audio shorter than its sample count", &|d| d["fillers"][0]["pcm_b64"] = json!("AAAA"), "pcm_b64"),
        ("a loudness above 1", &|d| d["fillers"][0]["envelope"][3] = json!(1.5), "outside 0..1"),
        ("a filler with no audio", &|d| _ = d["fillers"][0].as_object_mut().map(|f| f.remove("pcm_b64")), "pcm_b64"),
    ];
    for (what, edit, reason) in refused {
        match with(edit) {
            Ok(_) => c.that(false, format!("{what} is refused")),
            Err(why) => c.that(why.contains(reason), format!("{what} is refused, saying {reason}: {why}")),
        }
    }
    c.that(voice::parse_bank(b"{ not json").is_err_and(|w| w.contains("not a filler bank")), "and a file that is not JSON");
}

/// Docs/DESIGN_Characters.md 5: her own bank when she has one; none when she has her own voice and
/// no bank, because the default bank is 小毛's voice; the default bank when she has neither. A
/// bank that cannot be used is no bank, and its reason comes back to be logged.
fn each_character_plays_the_fillers_the_design_gives_her(c: &mut Checks) {
    let good = || Ok(Some(bank_bytes(json!([bank_filler("line_0", 30), bank_filler("line_1", 40)]))));
    let bad = || Ok(Some(b"{\"sample_rate\":8000}".to_vec()));
    let names = |bank: &voice::Bank| bank.fillers().iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    let default: Vec<String> = voice::bank().iter().map(|f| f.name.clone()).collect();

    for has_voice in [true, false] {
        let (bank, why) = voice::Bank::of(good(), has_voice);
        c.eq(names(&bank), vec!["line_0".to_string(), "line_1".to_string()], format!("her own bank is hers, voice of her own: {has_voice}"));
        c.eq(why, None, "and there is nothing to log");
    }

    let (bank, why) = voice::Bank::of(Ok(None), true);
    c.eq(names(&bank), Vec::<String>::new(), "a voice of her own and no bank: no filler");
    c.that(voice::pick(bank.fillers(), 0).is_none(), "so the page is answered nothing and plays nothing");
    c.eq(why, None, "and a file she does not have is not a fault");
    let (bank, _) = voice::Bank::of(Ok(None), false);
    c.eq(names(&bank), default.clone(), "neither a voice nor a bank: the default bank");

    let (bank, why) = voice::Bank::of(bad(), true);
    c.eq(names(&bank), Vec::<String>::new(), "a broken bank with a voice of her own is no filler");
    c.that(why.as_deref().is_some_and(|w| w.contains("not a filler bank")), format!("and says why: {why:?}"));
    let (bank, why) = voice::Bank::of(bad(), false);
    c.eq(names(&bank), default, "a broken bank in the default voice falls back to the default bank");
    c.that(why.is_some(), "and says why");
    let (bank, why) = voice::Bank::of(Err("fillers.json 有 1.0 MB，超過 1 MB 的上限".to_string()), true);
    c.that(matches!(bank, voice::Bank::Quiet), "a bank that may not be read counts as none");
    c.that(why.as_deref().is_some_and(|w| w.contains("超過")), "and the refusal is what is logged");
}

/// Her bank is read when she is first asked for a filler and not again while she is the one shown;
/// another character is read when she is, and only one bank is held. The rotation goes on across
/// the reads, because the turn counter is the app's and not the bank's.
fn a_bank_is_read_once_per_shown_character(c: &mut Checks) {
    let shown = voice::Shown::default();
    let reads = std::cell::Cell::new(0);
    let own = || {
        reads.set(reads.get() + 1);
        voice::Bank::Own(voice::parse_bank(&bank_bytes(json!([bank_filler("line_0", 30), bank_filler("line_1", 40)]))).unwrap_or_default())
    };
    let said: Vec<Option<String>> = (0..3).map(|turn| shown.pick("good", turn, own).map(|f| f.name)).collect();
    c.eq(said, vec![Some("line_0".to_string()), Some("line_1".to_string()), Some("line_0".to_string())], "three turns go round her bank");
    c.eq(reads.get(), 1, "and her bank was read once for all three");
    c.eq(shown.pick("other", 3, || {
        reads.set(reads.get() + 1);
        voice::Bank::Quiet
    }).map(|f| f.name), None, "another character is read when she is shown");
    c.eq(reads.get(), 2, "once");
    c.that(shown.pick("good", 4, own).is_some(), "and the first again");
    c.eq(reads.get(), 3, "is read again, because only the one on screen is kept");
}

/// A ten-second voiced line as the core writes it: 160000 samples is 320000 bytes, which is 426668
/// characters of base64. The characters are not real audio; this file never decodes them.
fn voiced_line(b64_chars: usize) -> String {
    let envelope = vec!["0.5"; 500].join(",");
    format!(
        r#"{{"ev":"utterance","text":"[nod]好","tags":["nod"],"voice":{{"state":"voiced","samples":160000,"pcm_b64":"{}","envelope":[{envelope}]}}}}"#,
        "A".repeat(b64_chars)
    )
}

/// The restatement's one addition: a voiced utterance reaches the page with `plan` beside the
/// fields the core wrote, every one of them untouched, and every other line reaches it as written.
fn a_voiced_utterance_gains_a_plan_and_keeps_every_field(c: &mut Checks) {
    let b = &profile::figure().bindings;
    let event = voiced_event(HAND_TEXT, &hand_samples());
    let page = crate::core::for_page(event.clone(), b);
    let (Some(sent), Some(got)) = (event.as_object(), page.as_object()) else {
        return c.that(false, "both are objects");
    };
    for (key, value) in sent {
        c.eq(got.get(key), Some(value), format!("`{key}` is the core's"));
    }
    c.eq(got.len(), sent.len() + 1, "one field is added");
    c.that(
        page.pointer("/plan/actions").is_some_and(Value::is_array) && page.pointer("/plan/dispositions").is_some_and(Value::is_array),
        "and it is the plan: actions and dispositions",
    );
    let first = page.pointer("/plan/actions/0").cloned().unwrap_or(Value::Null);
    c.that(
        first["at"].is_number() && first["action"]["do"] == "driveParams" && first["action"]["lanes"][0]["rise"].is_number(),
        format!("an action is a stroke time and lanes with their spans: {first}"),
    );

    let broken = json!({ "ev": "utterance", "text": "好", "voice": { "state": "voiced", "samples": 2, "pcm_b64": "!!!!" } });
    c.that(
        crate::core::for_page(broken, b).pointer("/plan/error").is_some_and(Value::is_string),
        "audio that cannot be read is a plan that says so, not a missing one",
    );
    for line in [r#"{"ev":"ready","version":"0.1.0"}"#, r#"{"ev":"turn_end"}"#, r#"{"ev":"error","message":"x"}"#] {
        let v: Value = serde_json::from_str(line).expect("a literal line");
        c.eq(crate::core::for_page(v.clone(), b), v, format!("{line} is relayed as it is"));
    }
}

/// An answer comes in parts, and each part is an `utterance` with a `part` number (the restatement at
/// the top of `core.rs`). The pet reads the number and does not write it, so the whole of this file's
/// job is to let it through: the line is accepted as it is, the number reaches the page beside the
/// fields it came with, and a line with no number is still a whole turn.
fn a_part_number_is_accepted_and_forwarded(c: &mut Checks) {
    let b = &profile::figure().bindings;
    for (line, part) in [
        (r#"{"ev":"utterance","part":0,"text":"[nod]好熱喔，","tags":["nod"],"voice":{"state":"silent","why":"x"}}"#, Some(0)),
        (r#"{"ev":"utterance","part":2,"text":"喝點冰的。","tags":[],"voice":{"state":"silent","why":"x"}}"#, Some(2)),
        (r#"{"ev":"utterance","text":"沒有編號","tags":[],"voice":{"state":"silent","why":"x"}}"#, None),
    ] {
        match parse_line(line.as_bytes()) {
            Err(why) => c.that(false, format!("{line} parses: {why}")),
            Ok(event) => {
                c.eq(event.get("part").and_then(Value::as_u64), part, format!("the number in {line} is read as it was written"));
                c.eq(Some(event.clone()), serde_json::from_str::<Value>(line).ok(), "and the payload is the line, whole");
                c.eq(crate::core::for_page(event.clone(), b), event, "a silent part reaches the page as the core wrote it, number and all");
            }
        }
    }

    let mut voiced = voiced_event("[open]喝點冰的。", &clip(80, |f| if (4..60).contains(&f) { 6554 } else { 0 }));
    voiced["part"] = json!(1);
    let page = crate::core::for_page(voiced.clone(), b);
    let (Some(sent), Some(got)) = (voiced.as_object(), page.as_object()) else {
        return c.that(false, "both are objects");
    };
    c.eq(got.get("part"), Some(&json!(1)), "a voiced part reaches the page with its number");
    for (key, value) in sent {
        c.eq(got.get(key), Some(value), format!("`{key}` of a voiced part is the core's"));
    }
    c.eq(got.len(), sent.len() + 1, "and `plan` is the one field added, as for a turn of one part");
    c.that(page.pointer("/plan/actions").is_some_and(Value::is_array), "a part after the first is planned like any voiced line");

    c.that(
        CORE.contains(r#"{"ev":"utterance","text","tags","voice","part"}"#),
        "the restatement at the top of core.rs names the part number",
    );
}

/// The two lines that make her someone, against the lines ChatCore's own tests send
/// (ChatCore/src/protocol.rs `Wire`). Compared as JSON, so the order serde writes keys in is not
/// part of the claim. The persona carries a newline and quotes, which must not end the line. She
/// wears an outfit, which the core is never told: it is the settings list's, and she is the same
/// person in it, so the line is the wire with no `outfit` in it.
fn the_character_and_greet_lines_are_the_cores_wire(c: &mut Checks) {
    let speaking = |voice: Option<&str>| cast::Speaking {
        profile: cast::Profile {
            name: "阿好".to_string(),
            outfit: Some("禮服".to_string()),
            persona: "好人。\n說話有\"引號\"".to_string(),
            greeting: Some("你好。".to_string()),
            voice: voice.map(|_| cast::VOICE.to_string()),
            fillers: vec!["嗯".to_string()],
        },
        voice: voice.map(|id| cast::Id::parse(id).expect("a valid id")),
    };
    let as_json = |line: &str| serde_json::from_str::<Value>(line).unwrap_or(Value::Null);

    let line = crate::core::character_line(&speaking(Some("good")));
    c.that(line.ends_with('\n') && line.matches('\n').count() == 1, format!("the character line is one line: {line:?}"));
    c.eq(
        as_json(&line),
        as_json(r#"{"op":"character","name":"阿好","persona":"好人。\n說話有\"引號\"","voice":"good"}"#),
        "with her name, her persona exactly, and her id as the voice, and nothing of the profile the core has no use for",
    );

    let line = crate::core::character_line(&speaking(None));
    c.eq(
        as_json(&line),
        as_json(r#"{"op":"character","name":"阿好","persona":"好人。\n說話有\"引號\""}"#),
        "with no voice of her own the voice is left out",
    );
    c.that(as_json(&line).get("voice").is_none(), "left out and not null");

    let line = crate::core::greet_line("嗨，我在這裡。");
    c.that(line.ends_with('\n') && line.matches('\n').count() == 1, "the greet line is one line");
    c.eq(as_json(&line), as_json(r#"{"op":"greet","text":"嗨，我在這裡。"}"#), "and carries the greeting exactly");
}

fn both_voices_come_through_whole(c: &mut Checks) {
    let line = voiced_line(426_668);
    c.that(line.len() > 426_668, format!("the line is the size of ten seconds of speech, {} bytes", line.len()));
    let began = std::time::Instant::now();
    let parsed = parse_line(line.as_bytes());
    let took = began.elapsed();
    match parsed {
        Err(why) => c.that(false, format!("a voiced utterance parses: {why}")),
        Ok(event) => {
            let voice = event.get("voice").cloned().unwrap_or(Value::Null);
            c.eq(voice.get("state").and_then(Value::as_str), Some("voiced"), "the voice is voiced");
            c.eq(voice.get("samples").and_then(Value::as_u64), Some(160_000), "with its sample count");
            c.eq(
                voice.get("pcm_b64").and_then(Value::as_str).map(str::len),
                Some(426_668),
                "and every character of the audio",
            );
            c.eq(voice.get("envelope").and_then(Value::as_array).map(Vec::len), Some(500), "and every envelope frame");
            c.eq(Some(event), serde_json::from_str::<Value>(&line).ok(), "the payload is the line, whole");
        }
    }
    c.that(took < std::time::Duration::from_millis(200), format!("parsing it takes {took:?}, not long enough to stall the reader"));

    let silent = r#"{"ev":"utterance","text":"好","tags":[],"voice":{"state":"silent","why":"本機語音服務沒有開"}}"#;
    match parse_line(silent.as_bytes()) {
        Err(why) => c.that(false, format!("a silent utterance parses: {why}")),
        Ok(event) => c.eq(
            event.get("voice"),
            Some(&json!({ "state": "silent", "why": "本機語音服務沒有開" })),
            "a silent voice arrives with its reason and nothing else",
        ),
    }
}

/// The answer's audio goes through the player the filler uses, so it has the same decode, the same
/// speakers and the same mouth. A second player would be a second mouth source, and the page
/// registers one.
fn the_answer_plays_through_the_same_player(c: &mut Checks) {
    c.eq(PAGE.matches("createBufferSource(").count(), 1, "the page has one place that makes a sound");
    c.eq(PAGE.matches("new (window.AudioContext").count(), 1, "and one audio context");
    let utterance: &str = PAGE
        .find("if (p.ev === \"utterance\") {")
        .and_then(|at| PAGE[at..].split_once("} else if (p.ev === \"error\")").map(|(body, _)| body))
        .unwrap_or("");
    c.that(!utterance.is_empty(), "the page has an utterance branch");
    let stop = utterance.find("Speaker.stop();");
    let voiced = utterance.find("voice.state === \"voiced\"");
    let speak = utterance.find("Speaker.speak(voice, p.plan)");
    c.that(
        matches!((stop, voiced, speak), (Some(a), Some(b), Some(s)) if a < b && b < s),
        format!("an answer stops what is sounding, then a voiced one is spoken: {stop:?} {voiced:?} {speak:?}"),
    );
    c.that(
        utterance.contains("if (!silenceExplained) {") && utterance.contains("silenceExplained = true;"),
        "a silent answer says why once a session, in its own bubble",
    );

    // A filler is fetched from Rust before it plays. One that comes back after the answer started
    // must not cut the answer off, so the ticket is checked after each wait and before the sound.
    let module: &str = PAGE
        .find("const Speaker = (function () {")
        .and_then(|at| PAGE[at..].split_once("\n})();").map(|(body, _)| body))
        .unwrap_or("");
    let filler: &str = module
        .find("async function filler()")
        .and_then(|at| module[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    c.that(
        filler.contains("const mine = ++ticket;") && filler.contains("mine !== ticket"),
        "a filler takes a ticket and gives up when a newer one was taken while it was fetched",
    );
    let start: &str = module
        .find("async function start(")
        .and_then(|at| module[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    let resumed = start.find("await context().resume()");
    let checked = start.find("if (mine !== ticket) return false;");
    let sounded = start.find("src.start(at)");
    c.that(
        matches!((resumed, checked, sounded), (Some(r), Some(k), Some(s)) if r < k && k < s),
        format!("and the ticket is checked after the context wakes and before the sound starts: {resumed:?} {checked:?} {sounded:?}"),
    );
    let stop: &str = module
        .find("function stop() {")
        .and_then(|at| module[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    c.that(
        stop.contains("ticket++;") && stop.contains("silence();"),
        "stopping takes a ticket too, so nothing still on its way plays after it",
    );
}

/// The text of one function of the page's `Speaker` module, from its declaration to the line that
/// closes it. `head` is the declaration as written, such as `function queue(`. Empty when the module
/// has no such function, which every caller turns into a failed check rather than a vacuous one.
fn speaker_fn(head: &str) -> String {
    let page = PAGE.replace("\r\n", "\n");
    let module = page
        .find("const Speaker = (function () {")
        .and_then(|at| page[at..].split_once("\n})();").map(|(body, _)| body))
        .unwrap_or("");
    module
        .find(&format!("\n  {head}"))
        .and_then(|at| module[at + 1..].split_once("\n  }").map(|(body, _)| body.to_string()))
        .unwrap_or_default()
}

/// The parts of one answer are scheduled on the audio clock, each at the end of what is already
/// scheduled, so a part that arrives in time joins the one before it at the sample, and one that
/// arrives after the turn went quiet starts at once. The clock decides whether the turn is still
/// going and not `onended`, which fires some time after the sound has stopped.
///
/// Pinned as text, because this crate cannot run the page. The same module runs against a fake
/// audio context in the harness the report names.
fn later_parts_are_scheduled_back_to_back_on_the_audio_clock(c: &mut Checks) {
    let start = speaker_fn("async function start(");
    c.that(!start.is_empty(), "Speaker has start");
    c.that(
        start.contains("const at = Math.max(now, turn.end);"),
        "a part starts at the end of what is scheduled, or now when that is already past",
    );
    c.that(start.contains("src.start(at);"), "at that time on the audio clock, and not whenever the page gets to it");
    c.that(
        start.contains("end: at + src.buffer.duration") && start.contains("turn.end = part.end;"),
        "and the turn then ends where that part does, so the next one is placed after it",
    );
    let joins = start.find("if (!(joins && playing && now < playing.end)) {");
    let placed = start.find("const at = Math.max(now, turn.end);");
    c.that(
        matches!((joins, placed), (Some(j), Some(p)) if j < p),
        format!("a part joins a turn only while the clock says the turn is going, decided before it is placed: {joins:?} {placed:?}"),
    );
    let silenced = start.find("silence();");
    let opened = start.find("playing = { parts: [], startedAt: now, end: now, merged: null };");
    c.that(
        matches!((joins, silenced, opened), (Some(j), Some(s), Some(o)) if j < s && s < o),
        format!("any other clip stops the old turn and opens one of its own: {silenced:?} {opened:?}"),
    );
    c.that(
        speaker_fn("function speak(").contains("start(voice, mine, actionsOf(plan))"),
        "speak never joins, so the first part always opens the turn",
    );
    c.that(
        speaker_fn("function queue(").contains("start(voice, ticket, actionsOf(plan), true, onStart)"),
        "and queue always does",
    );
}

/// A later part is dropped when anything has stopped or replaced the turn it belongs to since the
/// first part: a filler, `Speaker.stop()`, or a newer `speak`. Each of those takes a ticket, `queue`
/// takes none, and a part is wanted only while the newest ticket is still the one `speak` took. The
/// ticket is looked at again once the context is running, so a part that was waiting for it when the
/// turn was stopped makes no source and sounds nothing.
fn a_later_part_is_dropped_when_its_turn_was_stopped_or_replaced(c: &mut Checks) {
    let speak = speaker_fn("function speak(");
    let queue = speaker_fn("function queue(");
    let filler = speaker_fn("async function filler(");
    let stop = speaker_fn("function stop() {");
    let start = speaker_fn("async function start(");
    let silence = speaker_fn("function silence() {");
    for (name, body) in [("speak", &speak), ("queue", &queue), ("filler", &filler), ("stop", &stop), ("start", &start), ("silence", &silence)] {
        c.that(!body.is_empty(), format!("Speaker has {name}"));
    }
    c.that(
        speak.contains("const mine = ++ticket;") && speak.contains("answer = mine;"),
        "speak opens the answer to later parts under the ticket it just took",
    );
    c.eq(PAGE.matches("answer = mine;").count(), 1, "and nothing else does");
    c.that(
        PAGE.contains("let answer = null;"),
        "and before any speak there is no answer, since 0 is the first ticket and a part with no first part would be taken for one",
    );
    c.that(
        !filler.contains("answer") && filler.contains("++ticket") && stop.contains("ticket++;"),
        "a filler is a turn of one part that opens nothing, and it and a stop take a ticket, which closes the answer",
    );
    c.that(
        queue.contains("if (answer !== ticket) return Promise.resolve(false);"),
        "a later part is wanted only while no ticket has been taken since speak took its own",
    );
    c.that(
        !queue.contains("++ticket") && !queue.contains("ticket++"),
        "and it takes none of its own, which would close the turn it belongs to",
    );
    let resumed = start.find("await context().resume()");
    let checked = start.find("if (mine !== ticket) return false;");
    let made = start.find("createBufferSource()");
    let sounded = start.find("src.start(at)");
    c.that(
        matches!((resumed, checked, made, sounded), (Some(r), Some(k), Some(m), Some(s)) if r < k && k < m && m < s),
        format!(
            "a part that waited for the context is checked against the ticket again before it makes or starts a source: {resumed:?} {checked:?} {made:?} {sounded:?}"
        ),
    );
    c.that(
        silence.contains("playing.parts.forEach((part) => {") && silence.contains("part.src.stop();"),
        "and stopping cancels every part, the ones scheduled ahead as well as the one sounding",
    );
}

// ---------------------------------------------------------------------------------------------
// pet_mouth: the mouth follows the loudness the voice ships with.
//
// Static guards on the shipped text, because this crate cannot run JS. What the code does is
// proven where it can run: `sampleMouth` in the headless report reads the parameter back from
// inside the frame, and a harness runs the real `Filler.level` against a real AudioContext. What
// these catch is the wiring being refactored away, which looks exactly like the feature never
// having been built. `only_one_file_draws_the_figure` already covers the separation between the page
// and `character.js`, so nothing here repeats it.

/// The mouth is written to the parameter the rig names for it, from one place.
///
/// Measured 2026-11-05 on Mao (Docs/DESIGN_Pet.md 4.3.1): the usual name was not the model's, and
/// writing it left the mouth still with a clean console. So the id is not a literal in
/// `character.js`. It is the rig's own `lipSync`, the rig declares it with a range the voice's 0..1
/// can open, and the open mouth is shown by that parameter and no other.
fn the_mouth_is_written_to_the_parameter_the_rig_names(c: &mut Checks) {
    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let id = rig["lipSync"].as_str().unwrap_or("");
    c.eq(id, "ParamMouthOpenY", "rig.json names the mouth's parameter");
    let declared = rig["parameters"].as_array().and_then(|ps| ps.iter().find(|p| p["id"] == id)).cloned();
    c.that(
        matches!(&declared, Some(p) if p["min"] == 0 && p["max"] == 1),
        format!("and declares it, 0 to 1: {declared:?}"),
    );
    c.eq(rig.pointer("/layers/mouth_open/show/param"), Some(&json!(id)), "the open mouth shows as it rises");
    c.eq(rig.pointer("/layers/mouth_closed/show/param"), Some(&json!(id)), "and the closed one goes as it rises");

    c.that(CHARACTER.contains("const id = rig.lipSync;"), "character.js takes the id from the rig");
    c.that(!CHARACTER.contains(&format!("\"{id}\"")), "and never spells it itself");
    c.eq(
        CHARACTER.matches("params[mouthWiring.index].value = ").count(),
        1,
        "the mouth parameter is written from exactly one place",
    );
    c.that(
        CHARACTER.contains("mouthWiring.why = \"the rig has no parameter "),
        "and a rig without it says so in status instead of leaving the mouth still",
    );
    c.that(
        CHARACTER.contains("mouth: mouth,") && CHARACTER.contains("sampleMouth: sampleMouth,"),
        "character.js exports the source registration and the probe",
    );
}

/// The body of the function in `character.js` that starts with `head`, up to its closing brace.
fn character_fn(head: &str) -> String {
    let source = CHARACTER.replace("\r\n", "\n");
    source
        .find(&format!("\n  {head}"))
        .and_then(|at| source[at + 1..].split_once("\n  }").map(|(body, _)| body.to_string()))
        .unwrap_or_default()
}

/// The mouth is the last thing written before the figure is drawn, and the frame is one function
/// that says so.
///
/// On Mao this was a Cubism event, `beforeModelUpdate`, because the expressions wrote the mouth after
/// everything else had run (Docs/DESIGN_Pet.md 4.3.1). The puppet has no expressions, but the idle
/// sets every parameter and the gestures blend over it, so a mouth written any earlier would be
/// written over and the console would stay clean.
fn the_mouth_is_written_last_before_the_figure_is_drawn(c: &mut Checks) {
    let frame = character_fn("function frame(now) {");
    let order = ["idle(now / 1000);", "writeGestures();", "physics(dt);", "const said = writeMouth();", "draw();"];
    let at: Vec<Option<usize>> = order.iter().map(|step| frame.find(step)).collect();
    c.that(
        at.iter().all(Option::is_some) && at.windows(2).all(|w| w[0] < w[1]),
        format!("frame() runs the idle, the gestures, the hair, the mouth, then draws: {at:?}"),
    );
    c.eq(CHARACTER.matches("writeMouth()").count(), 2, "writeMouth is defined once and called once, by the frame");
    c.that(
        matches!((frame.find("draw();"), frame.find("if (mouthTap && said !== undefined) mouthTap(said);")), (Some(d), Some(t)) if d < t),
        "and the probe reads the mouth back once the figure is drawn",
    );
    c.that(
        frame.trim_start().starts_with("function frame(now) {\n    requestAnimationFrame(frame);"),
        "the next frame is asked for first, so one throw cannot stop the figure",
    );
}

/// The page registers its source once, and the source is the clip's own envelope read off the
/// audio clock.
fn the_page_hands_the_character_a_source_that_follows_the_clip(c: &mut Checks) {
    let mount = PAGE.find("Character.mount(");
    let register = PAGE.find("Character.mouth(Speaker.level);");
    c.that(register.is_some(), "the page registers Speaker.level as the mouth source");
    c.eq(PAGE.matches("Character.mouth(").count(), 1, "and registers it once");
    c.that(
        matches!((mount, register), (Some(m), Some(r)) if r > m),
        format!("after the character is on its way: {mount:?} then {register:?}"),
    );

    let module: &str = PAGE
        .find("const Speaker = (function () {")
        .and_then(|at| PAGE[at..].split_once("\n})();").map(|(body, _)| body))
        .unwrap_or("");
    c.that(module.contains("function level()"), "Speaker has a level function");
    c.that(module.contains("return { filler, speak, queue, stop, level,"), "and exposes it");
    // A turn is its parts, so the end of one part is the end of the turn only when no part comes
    // after it. A part that clears a turn with another one scheduled behind it would cut the
    // answer's mouth, gestures and `playing` off in the middle of what is still to be said.
    c.that(
        module.contains("src.onended = () => { if (playing === turn && turn.parts[turn.parts.length - 1] === part) playing = null; };"),
        "the end of a part clears the turn only if it is still the current one and this was its last part",
    );
    let silence: &str = module
        .find("function silence()")
        .and_then(|at| module[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    c.that(silence.contains("playing = null;"), "and silencing it clears it");

    // The position is the audio clock's. A timer started beside `start()` is late by however late
    // the page ran it, which is exactly the lag a mouth must not have.
    let level: &str = module
        .find("function level()")
        .and_then(|at| module[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    c.that(level.contains("ctx.currentTime"), "level() reads the AudioContext clock");
    c.that(
        !level.contains("performance.now") && !level.contains("Date.now"),
        "and not a timer of the page's own",
    );

    c.that(
        PAGE.contains("const mouth = await Character.sampleMouth(600);") && PAGE.contains("      mouth,"),
        "the headless report carries the mouth probe",
    );
}

/// Rust and the page agree on the length of one envelope frame.
///
/// The envelope is one value per `FRAME_SAMPLES` samples at `SAMPLE_RATE`, so a frame is 20 ms, and
/// the page turns an audio-clock position into a frame index by dividing by that. The two numbers
/// are written once on each side. The page derives the 20 ms from them and does not write it, so
/// this case checks the page holds the same pair Rust does and divides them, and that the rate has
/// no second copy elsewhere to drift from the first.
fn rust_and_the_page_agree_on_the_frame_length(c: &mut Checks) {
    let frame_seconds = voice::FRAME_SAMPLES as f64 / voice::SAMPLE_RATE as f64;
    c.eq(frame_seconds, 0.02, "a frame is 20 ms in Rust");

    c.that(
        PAGE.contains(&format!("const SAMPLE_RATE = {};", voice::SAMPLE_RATE)),
        "the page's sample rate is Rust's",
    );
    c.that(
        PAGE.contains(&format!("const FRAME_SAMPLES = {};", voice::FRAME_SAMPLES)),
        "and its frame length in samples is Rust's",
    );
    c.that(
        PAGE.contains("const FRAME_SECONDS = FRAME_SAMPLES / SAMPLE_RATE;"),
        "and the frame length in seconds is derived from the two, not written down",
    );
    c.eq(
        PAGE.matches(&voice::SAMPLE_RATE.to_string()).count(),
        1,
        "the sample rate is written once on the page, so the decode and the clock cannot disagree",
    );

    // The same 20 ms against the data and not against the constants: the packer measured each
    // clip's length itself, so a bank cut at some other frame length would not add up to it.
    for f in voice::bank() {
        let spanned = f.envelope.len() as f64 * frame_seconds;
        c.that(
            (spanned - f64::from(f.secs)).abs() < 1e-3,
            format!("{}: {} frames of 20 ms span {spanned:.3} s, the clip is {} s", f.name, f.envelope.len(), f.secs),
        );
    }
}

/// The loudness stays measured and the face does its own reading of it.
///
/// The envelope is RMS amplitude, and the same envelope is what Unreal's face reads, where it gates
/// a solver rather than opening a parameter (VRSpeechFace.h). So a curve that makes the mouth
/// readable on this renderer belongs to this renderer. Measured 2026-11-05: written straight
/// through, the six shipped clips put the mouth at a mean of 0.14 to 0.36 and it reads as not
/// moving; `character.js` applies the curve and `Filler.level` does not.
fn the_loudness_is_shaped_by_the_face_and_not_in_the_envelope(c: &mut Checks) {
    c.that(CHARACTER.contains("function opening(level)"), "character.js has the curve");
    c.that(
        CHARACTER.contains("Math.max(opening(given), mouthWiring.low)"),
        "and the written value goes through it",
    );

    let level: &str = PAGE
        .find("function level()")
        .and_then(|at| PAGE[at..].split_once("\n  }").map(|(body, _)| body))
        .unwrap_or("");
    c.that(!level.is_empty(), "the page still has level()");
    c.eq(
        reaches_runtime(level, &["Math.sqrt", "Math.pow", "Math.log10"]),
        Vec::<String>::new(),
        "and it hands over the loudness it measured, with no curve of its own",
    );
}

/// A turn is several clips, each with its own envelope, and the mouth follows whichever one is
/// sounding at the instant `level()` is asked. The turn has no envelope of its own: reading one
/// would hold the first part's loudness over the second part's speech.
///
/// Pinned as text. The module runs against a fake audio context in the harness the report names,
/// and the real window is where the mouth is seen.
fn the_mouth_reads_the_envelope_of_the_part_sounding_now(c: &mut Checks) {
    let level = speaker_fn("function level() {");
    let sounding = speaker_fn("function sounding(now) {");
    c.that(!level.is_empty() && !sounding.is_empty(), "Speaker has level and the choice of part it reads");
    c.that(
        level.contains("const part = sounding(ctx.currentTime);"),
        "level asks which part is sounding at the audio clock's now",
    );
    c.that(
        level.contains("const envelope = part.envelope;") && level.contains("(ctx.currentTime - part.at) / FRAME_SECONDS"),
        "and reads that part's envelope, at the time since that part started",
    );
    c.that(
        !level.contains("playing.envelope") && !level.contains("playing.startedAt"),
        "never the turn's own, since the turn has neither",
    );
    c.that(
        sounding.contains("playing.parts[k].at <= now") && sounding.contains("k--"),
        "the part sounding is the last one that has started, which is the earlier one between two parts",
    );
}

// ---------------------------------------------------------------------------------------------
// pet_gesture: the body moves with what the character says (Docs/DESIGN_Pet.md 4.1).

/// The reply worked by hand from VRGesturePlan.cpp, with the arithmetic in Docs/DESIGN_Pet.md 4.1.2.
/// Three clauses: a nod on the greeting, an open gesture on the second, and a thinking glance on the
/// question.
const HAND_TEXT: &str = "[nod]你好，[open]我們一起看看。[think]今天好嗎？";

/// One 20 ms frame of a square wave at `amplitude` of full scale: its RMS is the amplitude exactly,
/// which is what lets the arithmetic be done by hand.
fn square_frame(amplitude: i16) -> impl Iterator<Item = f32> {
    (0..voice::FRAME_SAMPLES).map(move |k| f32::from(if k % 2 == 0 { amplitude } else { -amplitude }) / 32768.0)
}

/// A clip of `frames` frames, silent except where `level` says.
fn clip(frames: usize, level: impl Fn(usize) -> i16) -> Vec<f32> {
    (0..frames).flat_map(|f| square_frame(level(f))).collect()
}

/// The hand case's audio: voiced at 0.2 of full scale over frames 5-29, 40-199 and 220-299, with
/// six single louder frames, which are the only frames loud enough to be a beat.
fn hand_samples() -> Vec<f32> {
    clip(310, |f| match f {
        15 | 192 => 16384,
        115 => 14746,
        150 => 19661,
        240 => 18022,
        280 => 13107,
        5..=29 | 40..=199 | 220..=299 => 6554,
        _ => 0,
    })
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 2e-5
}

fn every_kind_moves() -> Bindings {
    all_chosen()
}

/// A cue as `(kind, at)`, with a nod's degrees or a glance's end, yaw and pitch after it.
fn spelled(cue: &Cue) -> (CueKind, Vec<f32>) {
    let numbers = [Some(cue.at()), cue.measure(Measure::Degrees), cue.end(), cue.measure(Measure::Yaw), cue.measure(Measure::Pitch)];
    (cue.kind(), numbers.into_iter().flatten().collect())
}

fn spelled_close(got: &[(CueKind, Vec<f32>)], want: &[(CueKind, Vec<f32>)]) -> bool {
    got.len() == want.len()
        && got.iter().zip(want).all(|((gk, gn), (wk, wn))| {
            gk == wk && gn.len() == wn.len() && gn.iter().zip(wn).all(|(&g, &w)| close(g, w))
        })
}

fn the_alignment_matches_the_hand_worked_case(c: &mut Checks) {
    let Some(a) = align::align(HAND_TEXT, &hand_samples(), voice::SAMPLE_RATE) else {
        return c.that(false, "the hand case aligns");
    };
    c.eq(a.said.iter().collect::<String>(), "你好，我們一起看看。今天好嗎？".to_string(), "the spoken text is the text without its tags");
    c.eq(
        a.tags.iter().map(|t| (t.at, t.name.as_str())).collect::<Vec<_>>(),
        vec![(0, "nod"), (3, "open"), (10, "think")],
        "each tag sits before the character it was written in front of",
    );
    c.eq(a.spans.clone(), vec![(0, 3), (3, 10), (10, 15)], "three clauses, each keeping its punctuation");
    // Boundary 0 is expected at 0.1 + 5.9 * 2/12 = 1.0833 and the pause at 0.6 is within 0.6 s of
    // it; boundary 1 is expected at 0.8 + 5.2 * 6/10 = 3.92 and taken at the pause at 4.0.
    let want_times = [(0.1, 0.6), (0.8, 4.0), (4.4, 6.0)];
    c.that(
        a.clause_times.len() == 3 && a.clause_times.iter().zip(want_times).all(|(&(x, y), (wx, wy))| close(x, wx) && close(y, wy)),
        format!("clause times {:?}, worked by hand {want_times:?}", a.clause_times),
    );
    // 你 is half a syllable into a two-syllable clause of 0.5 s; 我 half of six over 3.2 s; 今 half of
    // four over 1.6 s. The entry past the last character is the end of the speech.
    let want_chars = [(0, 0.225), (3, 0.8 + 3.2 / 12.0), (10, 4.6), (15, 6.0)];
    c.that(
        want_chars.iter().all(|&(i, t)| close(a.char_time[i], t)),
        format!("character times {:?} at {:?}", want_chars.map(|(i, _)| a.char_time[i]), want_chars),
    );
}

/// The whole C++ plan for the hand case against the port, with every kind bound so the overlap rule
/// has nothing pet-specific to skip.
fn the_plan_matches_the_cpp_worked_by_hand(c: &mut Checks) {
    use CueKind::*;
    let bindings = every_kind_moves();
    let cues = plan::cues(HAND_TEXT, &hand_samples(), voice::SAMPLE_RATE, &bindings);
    let (actions, d) = schedule(&cues, &bindings);
    let pick = |want: fn(&CueDisposition) -> bool| -> Vec<(CueKind, Vec<f32>)> {
        let mut out: Vec<_> = cues.iter().zip(&d).filter(|(_, d)| want(d)).map(|((cue, _), _)| spelled(cue)).collect();
        out.sort_by(|x, y| x.1[0].total_cmp(&y.1[0]));
        out
    };
    let hands = pick(|d| matches!(d, CueDisposition::Played { kind } if kind.occupies().exclusive()));
    let want_hands = vec![
        (Open, vec![1.166_667]),
        (Beat, vec![3.0]),
        (BeatBoth, vec![3.84]),
        (Beat, vec![4.8]),
        (Question, vec![5.8]),
    ];
    c.that(spelled_close(&hands, &want_hands), format!("gestures {hands:?}, the C++ by hand {want_hands:?}"));

    let lost: Vec<(f32, CueKind)> = cues
        .iter()
        .zip(&d)
        .filter_map(|((cue, _), d)| match d {
            CueDisposition::Suppressed { by, .. } => Some((cue.at(), *by)),
            _ => None,
        })
        .collect();
    // The beat at 2.3 loses to the louder one at 3.0, which strikes later: beats are placed loudest
    // first (VRGesturePlan.cpp:384), and stroke order would have kept 2.3 instead.
    let want_lost = [(0.3, Open), (2.3, Beat), (5.6, Question)];
    c.that(
        lost.len() == 3 && lost.iter().zip(want_lost).all(|(&(t, by), (wt, wby))| close(t, wt) && by == wby),
        format!("suppressed beats {lost:?}, the C++ by hand {want_lost:?}"),
    );

    let nods = pick(|d| matches!(d, CueDisposition::Played { kind: Nod }));
    let want_nods = vec![(Nod, vec![0.375, 7.0]), (Nod, vec![3.84, 3.0])];
    c.that(spelled_close(&nods, &want_nods), format!("nods {nods:?}, the C++ by hand {want_nods:?}"));

    // The C++ also glances over the pause after 看看。, from 3.95 to 4.75 s at 14.6 degrees, on top
    // of this one. The pet keeps glances apart (`glances_keep_apart_and_to_one_side`), so only the
    // `[think]` glance is left, with the C++ draws.
    let gazes = pick(|d| matches!(d, CueDisposition::Played { kind: LookAway }));
    let want_gazes = vec![(LookAway, vec![4.5, 5.6, 18.058_897, 7.510_841])];
    c.that(spelled_close(&gazes, &want_gazes), format!("glances {gazes:?}, the C++ by hand less the crowded one {want_gazes:?}"));
    c.eq(actions.len(), 8, "five gestures, two nods and one glance reach the page");
}

/// Beats are a function of the loudness alone: a frame at least 0.35 of the loudest and the loudest
/// within five frames either side.
fn beats_come_from_the_loudness_peaks(c: &mut Checks) {
    let samples = clip(160, |f| match f {
        20 => 16384,
        60 => 19661,
        64 => 18022,
        100 => 6717,
        5..=150 => 6554,
        _ => 0,
    });
    // Nothing bound, so nothing holds the hands and every candidate comes back as it was found.
    let cues = plan::cues("你好嗎今天怎麼樣", &samples, voice::SAMPLE_RATE, &Bindings::default());
    let beats: Vec<(f32, f32)> = cues
        .iter()
        .filter_map(|(cue, claim)| match claim {
            Claim::Stress(loudness) => Some((cue.at(), *loudness)),
            _ => None,
        })
        .collect();
    c.that(
        beats.len() == 2 && close(beats[0].0, 0.4) && close(beats[1].0, 1.2),
        format!("beats at frames 20 and 60 only, saw {beats:?}"),
    );
    c.that(
        beats.len() == 2 && close(beats[0].1, 0.5) && close(beats[1].1, 0.6),
        format!("each carrying its loudness, saw {beats:?}"),
    );
    c.that(
        cues.iter().all(|(cue, _)| cue.kind() == CueKind::Beat),
        "frame 64 is louder than the floor but not the loudest near it, and frame 100 is under 0.35 of the peak",
    );
}

fn the_same_reply_always_plans_the_same(c: &mut Checks) {
    let samples = hand_samples();
    let b = figure_bindings();
    let first = plan::plan(HAND_TEXT, &samples, voice::SAMPLE_RATE, b);
    let again = plan::plan(HAND_TEXT, &samples, voice::SAMPLE_RATE, b);
    c.that(!first.actions.is_empty(), "the hand case plans something on the figure");
    c.eq(&first, &again, "the same text and audio plan the same, glances included");
    c.eq(plan::seed(HAND_TEXT), 0xFE17_0D55, "the seed is FNV-1a of the text, as the hand computation has it");

    // The same timing, other words: 瞧 for 看 keeps every syllable count, so only the seed moves.
    let other = HAND_TEXT.replace("看看", "瞧瞧");
    let yaws = |text: &str| -> Vec<f32> {
        plan::cues(text, &samples, voice::SAMPLE_RATE, &every_kind_moves())
            .iter()
            .filter_map(|(cue, _)| cue.measure(Measure::Yaw))
            .collect()
    };
    let (mine, theirs) = (yaws(HAND_TEXT), yaws(&other));
    c.that(mine.len() == 1 && theirs.len() == 1 && mine != theirs, format!("another reply draws other glances: {mine:?} and {theirs:?}"));
}

/// Not in the C++ (`plan::keep_glances_apart`): one head looks one way at a time, and a head that
/// swings back across within a second reads as a twitch.
fn glances_keep_apart_and_to_one_side(c: &mut Checks) {
    let glances = |text: &str, samples: &[f32]| -> Vec<(f32, f32, f32)> {
        plan::cues(text, samples, voice::SAMPLE_RATE, &every_kind_moves())
            .into_iter()
            .filter(|(cue, _)| cue.kind() == CueKind::LookAway)
            .map(|(cue, _)| (cue.at(), cue.end().unwrap_or(f32::NAN), cue.measure(Measure::Yaw).unwrap_or(f32::NAN)))
            .collect()
    };
    // The hand case: the pause after 看看。 glances from 3.95 to 4.75 s, and the `[think]` on 今
    // from 4.5 s. The words asked for the second, so it is the one kept.
    let hand = glances(HAND_TEXT, &hand_samples());
    c.that(hand.len() == 1 && close(hand[0].0, 4.5), format!("of two overlapping glances only the [think] one is made: {hand:?}"));

    // Three sentences, so two pause glances, which the C++ sends to alternate sides.
    let three = clip(270, |f| match f {
        5..=60 | 80..=150 | 170..=260 => 6554,
        _ => 0,
    });
    let two = glances("你好。今天好嗎。我們走吧。", &three);
    let gap = if two.len() == 2 { two[1].0 - two[0].1 } else { f32::NAN };
    c.that((0.8..1.5).contains(&gap), format!("two pause glances {gap} s apart, close enough to swing across: {two:?}"));
    c.that(two.len() == 2 && two[0].2 * two[1].2 > 0.0, format!("and they go the same way: {two:?}"));
}

fn figure_bindings() -> &'static Bindings {
    &profile::figure().bindings
}

fn lanes_of(action: &CharacterAction) -> Vec<ParamLane> {
    match action {
        CharacterAction::DriveParams { lanes } => lanes.clone(),
        _ => Vec::new(),
    }
}

fn lane<'a>(lanes: &'a [ParamLane], id: &str) -> Option<&'a ParamLane> {
    lanes.iter().find(|l| l.id == id)
}

/// Docs/DESIGN_Pet.md 11, item 10: resolution lets the cue's own numbers through.
fn resolve_sizes_nods_and_glances_by_the_cue(c: &mut Checks) {
    let b = figure_bindings();
    let resolved = |cue: Cue| lanes_of(&vrcue::resolve(&cue, b).unwrap_or(CharacterAction::Nothing));
    let small = resolved(Cue::nod(1.0, 3.0).expect("a nod"));
    let deep = resolved(Cue::nod(1.0, 7.0).expect("a nod"));
    let (s, d) = (lane(&small, "ParamAngleY").map(|l| l.to), lane(&deep, "ParamAngleY").map(|l| l.to));
    c.that(
        matches!((s, d), (Some(s), Some(d)) if close(s, 3.0 * -1.7143) && close(d, 7.0 * -1.7143)),
        format!("a 3 degree nod dips {s:?} and a 7 degree one {d:?}"),
    );

    let left = resolved(Cue::look_away(2.0, 3.0, -15.0, -10.0).expect("a glance"));
    let right = resolved(Cue::look_away(2.0, 3.5, 12.0, 6.0).expect("a glance"));
    let to = |lanes: &[ParamLane], id: &str| lane(lanes, id).map(|l| l.to);
    c.that(
        matches!(to(&left, "ParamAngleX"), Some(v) if close(v, -9.0)) && matches!(to(&right, "ParamAngleX"), Some(v) if close(v, 7.2)),
        format!("the head turns the side the yaw says, 0.6 of its size: {:?} and {:?}", to(&left, "ParamAngleX"), to(&right, "ParamAngleX")),
    );
    // The eyes get there first and the head follows: the eyes' stroke is the cue's, the head's later.
    let lags = (lane(&right, "ParamEyeBallX").map(|l| l.lag), lane(&right, "ParamAngleX").map(|l| l.lag));
    c.that(matches!(lags, (Some(eyes), Some(head)) if eyes < head), format!("the eyes lead the head: lags {lags:?}"));
    c.that(
        matches!(to(&left, "ParamAngleY"), Some(v) if close(v, -6.0)) && matches!(to(&right, "ParamAngleY"), Some(v) if close(v, 3.6)),
        format!("and dips or lifts as the pitch says: {:?} and {:?}", to(&left, "ParamAngleY"), to(&right, "ParamAngleY")),
    );
    // Item 11: a glance is held for its own window, with the binding's ramps either side of it.
    let held = lane(&right, "ParamAngleX").map(|l| (l.rise, l.hold, l.fall));
    c.that(
        matches!(held, Some((r, h, f)) if close(r, 0.35) && close(h, 1.5) && close(f, 1.85)),
        format!("a 1.5 s glance holds 1.5 s and falls 0.35 s after: {held:?}"),
    );
    c.that(
        small.iter().chain(&left).all(|l| l.per.is_none()),
        "the page never receives a lane that still needs sizing",
    );

    // The two answers resolve has to keep apart are still apart.
    c.eq(vrcue::resolve(&plain(CueKind::SelfRef, 1.0), b), Ok(CharacterAction::Nothing), "declared absent is a deliberate Nothing");
    c.eq(
        vrcue::resolve(&plain(CueKind::Offer, 1.0), &Bindings::default()),
        Err(vrcue::Unresolved::Unbound),
        "unbound is still an error",
    );
}

fn the_profile_binds_every_kind_to_what_the_rig_has(c: &mut Checks) {
    let p = profile::figure();
    for (kind, binding) in p.bindings.iter() {
        let ok = match kind {
            // The arms swing from the shoulder and cannot bring a hand to the chest, and the puppet
            // has no second arm pose to swap in (Docs/DESIGN_Pet.md 4.1.1).
            CueKind::SelfRef => *binding == Binding::DeclaredAbsent,
            // The puppet's idle is its rest.
            CueKind::Rest => {
                *binding == Binding::Chosen { action: CharacterAction::Nothing, evidence: Evidence::AuthoredProfile }
            }
            _ => matches!(binding, Binding::Chosen { action: CharacterAction::DriveParams { .. }, evidence: Evidence::AuthoredProfile }),
        };
        c.that(ok, format!("{kind:?} is bound as the profile means it to be: {binding:?}"));
    }
    c.that(p.bindings.iter().all(|(_, b)| *b != Binding::Unbound), "no slot is Unbound");

    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let declared: Vec<(String, f64, f64)> = rig["parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| Some((q["id"].as_str()?.to_string(), q["min"].as_f64()?, q["max"].as_f64()?)))
        .collect();
    c.that(!declared.is_empty(), format!("rig.json declares its parameters: {} of them", declared.len()));
    for (kind, binding) in p.bindings.iter() {
        let Binding::Chosen { action, .. } = binding else { continue };
        for l in lanes_of(action) {
            let range = declared.iter().find(|(id, _, _)| *id == l.id).map(|(_, lo, hi)| (*lo, *hi));
            c.that(range.is_some(), format!("{kind:?} drives {}, which rig.json declares", l.id));
            // A pose past the end of its range is clamped, and the user sees less than the profile
            // says. A sized lane is per degree of its cue, so its own number is not the pose.
            if let (Some((lo, hi)), None, false) = (range, l.per, l.relative) {
                c.that(
                    (lo..=hi).contains(&f64::from(l.to)),
                    format!("{kind:?} poses {} at {}, inside its range {lo} to {hi}", l.id, l.to),
                );
            }
            c.that(
                l.per.is_none_or(|m| m.carried_by(kind)),
                format!("{kind:?} sizes {} by {:?}, a number its cues carry", l.id, l.per),
            );
            // What the hands reserve in the schedule covers what they show on screen: a lane
            // whose stroke lags may start later and end later, never outside the span.
            if kind.occupies().exclusive() {
                let span = kind.span();
                c.that(
                    !l.id.starts_with("ParamBodyAngle") || l.relative,
                    format!("{kind:?} leans the body by {} from where the idle has it, not to a pose", l.id),
                );
                let (from, to) = (l.lag - l.rise, l.lag + l.fall);
                c.that(
                    l.lag >= 0.0 && from >= -span.rise - 1e-6 && to <= span.fall + 1e-6,
                    format!("{kind:?} {} moves from {from} to {to} s, inside its kind's -{} to {}", l.id, span.rise, span.fall),
                );
            }
        }
    }

    // The profile is bound to the parameters a rig declares, not to its bytes: the numbers are in
    // the parameters' own units, so a rig that moves its nodes differently for the same parameters
    // (the gown, which holds a book) is still a body they were written for.
    c.eq(profile::parameters(RIG.as_bytes()), Ok(p.parameters.clone()), "the profile is for the parameters of the rig in the bundle");
    c.that(profile::bindings_for(RIG.as_bytes()).is_ok(), "and so the bundle's figure gets it");
    let mut spaced = RIG.as_bytes().to_vec();
    spaced.push(b' ');
    c.that(profile::bindings_for(&spaced).is_ok(), "a rig.json with other bytes and the same parameters still does");
    let mut rig: Value = serde_json::from_str(RIG).unwrap_or_default();
    if let Some(nodes) = rig.get_mut("nodes") {
        *nodes = json!([]);
    }
    c.that(
        profile::bindings_for(rig.to_string().as_bytes()).is_ok(),
        "and so does one whose nodes move differently, since the numbers are in the parameters' units",
    );
    let mut wider: Value = serde_json::from_str(RIG).unwrap_or_default();
    if let Some(first) = wider.pointer_mut("/parameters/0/max") {
        *first = json!(45);
    }
    c.that(profile::bindings_for(wider.to_string().as_bytes()).is_err(), "a parameter with another range does not");
    c.that(profile::bindings_for(b"not json").is_err(), "nor a rig that does not parse");
    let gown = std::fs::read(ui_dir().join("characters/char-20261107-gown/rig.json")).unwrap_or_default();
    c.that(profile::bindings_for(&gown).is_ok(), "the gown, which declares the same parameters, gets her gestures");
    c.that(
        cast::Index::parse(CAST.as_bytes()).is_ok_and(|index| cast::bundled_file(&index.default, cast::RIG) == profile::MODEL),
        "the figure the profile names is the one the bundle shows by default",
    );
}

/// A `[think]` on the first word glances from 0.1 s before it (VRGesturePlan.cpp:313), which is
/// before the clip when the voice starts at once. The cue starts at 0 instead of being refused.
fn a_time_before_the_clip_is_clamped_not_dropped(c: &mut Checks) {
    let samples = clip(40, |f| if f < 30 { 6554 } else { 0 });
    let bindings = every_kind_moves();
    let cues = plan::cues("[think]今天好嗎？", &samples, voice::SAMPLE_RATE, &bindings);
    let glance = cues.iter().find(|(cue, _)| cue.kind() == CueKind::LookAway).map(|(cue, _)| *cue);
    // 今 is half a syllable into four over 0.6 s, at 0.075 s, so the glance would start at -0.025.
    c.that(
        matches!(glance, Some(g) if g.at() == 0.0 && matches!(g.end(), Some(e) if close(e, 1.075))),
        format!("the glance starts at the clip's start and keeps its end: {glance:?}"),
    );
    let (_, d) = schedule(&cues, &bindings);
    c.that(d.contains(&CueDisposition::Played { kind: CueKind::LookAway }), format!("and it plays: {d:?}"));
}

/// Standard base64 with padding, for building a voiced line the way the core does.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn voiced_event(text: &str, samples: &[f32]) -> Value {
    let pcm: Vec<u8> = samples.iter().flat_map(|&s| ((s * 32768.0).round() as i16).to_le_bytes()).collect();
    json!({
        "ev": "utterance",
        "text": text,
        "tags": ["nod", "open", "think"],
        "voice": { "state": "voiced", "samples": samples.len(), "pcm_b64": base64(&pcm), "envelope": [] },
    })
}

/// DESIGN_Voice 3.2: a silent turn has no audio, so no timing and no gestures.
fn a_silent_turn_carries_no_plan(c: &mut Checks) {
    let b = figure_bindings();
    let silent = json!({ "ev": "utterance", "text": "[nod]好", "tags": ["nod"], "voice": { "state": "silent", "why": "本機語音服務沒有開" } });
    c.eq(crate::core::for_page(silent.clone(), b), silent, "a silent utterance reaches the page as the core wrote it");

    let voiced = crate::core::for_page(voiced_event(HAND_TEXT, &hand_samples()), b);
    let actions = voiced.pointer("/plan/actions").and_then(Value::as_array).map(Vec::len);
    c.that(matches!(actions, Some(n) if n > 0), format!("the same words voiced get a plan, {actions:?} actions"));
}

/// A question mark claims the hands after a tag does (VRGesturePlan.cpp:317 against :359), so an
/// `[offer]` that follows a question keeps them.
fn a_tag_keeps_the_hands_from_a_question_mark(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();
    let (_, d) = schedule(&[(plain(Question, 2.0), Claim::QuestionMark), (plain(Offer, 2.5), Claim::Tag)], &b);
    c.eq(d, vec![suppressed(Question, Offer), played(Offer)], "the later tag wins over the earlier question mark");
    // The same two as tags: then the earlier one wins, so the case above is about the claim.
    let (_, d) = schedule(&[(plain(Question, 2.0), Claim::Tag), (plain(Offer, 2.5), Claim::Tag)], &b);
    c.eq(d, vec![played(Question), suppressed(Offer, Question)], "two tags go by stroke");
}

fn the_louder_beat_is_placed_first(c: &mut Checks) {
    use CueKind::*;
    let b = all_chosen();
    let (_, d) = schedule(&[(plain(Beat, 1.0), Claim::Stress(0.4)), (plain(Beat, 1.3), Claim::Stress(0.6))], &b);
    c.eq(d, vec![suppressed(Beat, Beat), played(Beat)], "the louder, later beat keeps the hands");
    let (_, d) = schedule(&[(plain(Beat, 1.0), Claim::Stress(0.6)), (plain(Beat, 1.3), Claim::Stress(0.4))], &b);
    c.eq(d, vec![played(Beat), suppressed(Beat, Beat)], "and the louder, earlier one does too");
}

/// The page half, read from the files: there is no browser here.
///
/// On Mao the gestures went in at `afterMotionUpdate` so physics would see them: a body written
/// there swung the robe 2.00 where the same write later swung it 0.00 (Docs/DESIGN_Pet.md 4.3.2).
/// The puppet's physics is her hair, and the same holds: it reads the head after the gestures, or a
/// nod would leave the hair hanging still. What a gesture wrote must not outlive its frame either,
/// which on Mao took an undo and here is the idle setting every parameter back first.
fn the_gestures_are_written_where_physics_sees_them(c: &mut Checks) {
    let frame = character_fn("function frame(now) {");
    c.that(
        matches!(
            (frame.find("idle(now / 1000);"), frame.find("writeGestures();"), frame.find("physics(dt);")),
            (Some(i), Some(g), Some(h)) if i < g && g < h
        ),
        "the gestures blend over the idle, and the hair reads the head after both",
    );
    let idle = character_fn("function idle(t) {");
    c.that(
        matches!((idle.find("p.value = p.rest;"), idle.find("rig.idle.forEach(")), (Some(r), Some(m)) if r < m),
        "the idle puts every parameter back to its rest before it moves any",
    );
    c.that(CHARACTER.contains("wireGestures();"), "the gestures are wired when the character mounts");

    // What swings the hair is what the gestures move: a glance turns the head and a question tilts it.
    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let inputs: Vec<&str> = rig["physics"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|h| h["input"].as_array().into_iter().flatten().filter_map(|i| i["param"].as_str()))
        .collect();
    let driven = |kind: CueKind| -> Vec<String> {
        match profile::figure().bindings.iter().find(|(k, _)| *k == kind) {
            Some((_, Binding::Chosen { action, .. })) => lanes_of(action).into_iter().map(|l| l.id).collect(),
            _ => Vec::new(),
        }
    };
    for (kind, id) in [(CueKind::LookAway, "ParamAngleX"), (CueKind::Question, "ParamAngleZ")] {
        c.that(
            driven(kind).iter().any(|l| l == id) && inputs.contains(&id),
            format!("{kind:?} moves {id} and the hair follows it: the hair reads {inputs:?}"),
        );
    }
}

/// She turns toward the cursor on top of the idle, and the gestures blend over both, so a glance
/// away or a nod is not cancelled by where the mouse happens to be.
fn she_looks_toward_the_cursor_and_a_gesture_still_wins(c: &mut Checks) {
    let frame = character_fn("function frame(now) {");
    c.that(
        matches!(
            (frame.find("idle(now / 1000);"), frame.find("writeLook(dt);"), frame.find("writeGestures();")),
            (Some(i), Some(l), Some(g)) if i < l && l < g
        ),
        "the look is written after the idle and before the gestures",
    );
    c.that(CHARACTER.contains("    look: look,"), "the character takes a look source");

    // Rust sends the cursor under one name and the page listens for that name.
    let name = MAIN
        .lines()
        .find_map(|l| l.strip_prefix("const CURSOR_EVENT: &str = \"")?.strip_suffix("\";"))
        .unwrap_or("");
    c.that(!name.is_empty(), "main.rs names the cursor event");
    c.that(MAIN.contains("win.emit(CURSOR_EVENT, at)"), "the poll sends the cursor to the window");
    c.that(PAGE.contains(&format!("listen(\"{name}\", ")), format!("the page listens for {name:?}"));
    let mount = PAGE.find("Character.mount(").unwrap_or(usize::MAX);
    c.that(
        PAGE.matches("Character.look(").count() == 1 && PAGE.find("Character.look(").is_some_and(|i| i > mount),
        "the page hands the character its look source once, after it mounts",
    );

    // What turns her is the rig's to say, on parameters the rig has, and it turns her head and eyes.
    let rig: Value = serde_json::from_str(RIG).unwrap_or(Value::Null);
    let declared: Vec<&str> =
        rig["parameters"].as_array().into_iter().flatten().filter_map(|p| p["id"].as_str()).collect();
    let lanes: Vec<(&str, &str)> = rig["look"]["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| Some((k["param"].as_str()?, k["axis"].as_str()?)))
        .collect();
    for (id, axis) in &lanes {
        c.that(declared.contains(id), format!("look turns {id}, which the rig declares"));
        c.that(*axis == "x" || *axis == "y", format!("look turns {id} on x or y, not {axis:?}"));
    }
    for id in ["ParamAngleX", "ParamAngleY", "ParamEyeBallX"] {
        c.that(lanes.iter().any(|(l, _)| *l == id), format!("looking turns {id}"));
    }
}

/// The C++ plays a gesture on straight ramps (VRBodyLanguageComponent.cpp:113-129); the pet eases
/// both, which also makes the nod the C++ nod, a raised cosine (:203).
fn the_gesture_blend_eases_in_and_out(c: &mut Checks) {
    for (line, what) in [
        ("if (t > -rise && t <= 0) return 0.5 * (1 - Math.cos(Math.PI * (1 + t / rise)));", "a half-cosine rise to the stroke"),
        ("if (t > 0 && t <= hold) return 1;", "held after it"),
        ("if (t > hold && t < fall) return 0.5 * (1 + Math.cos(Math.PI * (t - hold) / (fall - hold)));", "and a half-cosine fall that ends `fall` after the stroke"),
        ("at: a.at + lag,", "a lane's stroke lands `lag` after the cue's"),
        ("if (!(w > 0)) return;", "a lane at W 0 is not written"),
        ("p.toward += w * lane.to;", "overlapping absolute lanes are averaged by W, so no lane takes over in one frame"),
        ("p.add += w * lane.to;", "relative lanes add up by W"),
        ("const target = p.pull > 0 ? p.toward / p.pull : idle;", "the target is that W-weighted mean"),
        (
            "const value = idle + p.reach * (target - idle) + p.add + (1 - p.w) * drift;",
            "reached by the largest absolute W from what the idle gave, plus the relative lanes, and the sway given way to the gesture by W",
        ),
        ("const fade = edge >= 1 ? 1 : 0.5 * (1 - Math.cos(Math.PI * edge));", "the sway fades in and out at the clip's ends"),
        ("Math.min(Math.max(value, p.low), p.high)", "inside the parameter's own range"),
        ("refusedLanes.add(l.id + \": the rig has no such parameter (\"", "a parameter the rig does not declare is refused"),
        ("refused: Array.from(refusedLanes),", "and the refusal is in status"),
    ] {
        c.that(CHARACTER.contains(line), what);
    }
}

fn the_page_hands_the_character_the_answers_timeline(c: &mut Checks) {
    let register = PAGE.find("Character.gestures(Speaker.timeline);");
    c.that(
        matches!((PAGE.find("Character.mount("), register), (Some(m), Some(r)) if r > m),
        "the page registers Speaker.timeline as the gesture source, after the character is on its way",
    );
    c.eq(PAGE.matches("Character.gestures(").count(), 1, "once");
    c.that(
        PAGE.contains("const part = { src, envelope: f.envelope, actions, at, end: at + src.buffer.duration };"),
        "the plan is part of the part, so a new turn drops the old plan with the old sound",
    );
    c.that(PAGE.contains("Speaker.speak(voice, p.plan)"), "the answer's plan goes in with its voice");
    c.that(
        PAGE.contains("length: playing.end - playing.startedAt }"),
        "and the turn's length, so the sway can fade out before the sound stops",
    );
    c.that(PAGE.contains("return (await start(f, mine, null)) ? f : null;"), "a filler has no plan");
    let timeline: &str =
        PAGE.find("function timeline()").and_then(|at| PAGE[at..].split_once("\n  }").map(|(body, _)| body)).unwrap_or("");
    c.that(
        timeline.contains("ctx.currentTime - playing.startedAt")
            && timeline.contains("if (!playing || !playing.parts.some((part) => part.actions)) return null;"),
        "the timeline is the turn's own audio clock, and nothing when no planned turn sounds",
    );
}

/// Each part's plan counts from that part's first sample, and the turn is one timeline: every
/// action's `at` moves by how long after the turn began its part started, and `length` is where
/// the last scheduled part ends. `character.js` then needs to know nothing about parts. It keeps
/// the lanes it worked out per array, so the page hands it the same array until a part is added.
///
/// Pinned as text. The module runs against a fake audio context in the harness the report names.
fn the_timeline_shifts_each_part_by_where_it_starts(c: &mut Checks) {
    let all = speaker_fn("function allActions(");
    let timeline = speaker_fn("function timeline() {");
    let start = speaker_fn("async function start(");
    c.that(!all.is_empty() && !timeline.is_empty(), "Speaker has allActions and timeline");
    c.that(
        all.contains("a.at + (part.at - turn.startedAt)"),
        "a later part's actions move by how long after the turn began that part started",
    );
    c.that(
        all.contains("Object.assign({}, a, { at:"),
        "on copies, so the plan Rust made is never changed",
    );
    c.that(
        all.contains("turn.parts.length === 1") && all.contains("? turn.parts[0].actions"),
        "a turn of one part hands over its plan itself, not a copy",
    );
    c.that(
        all.contains("if (!turn.merged) {") && start.contains("turn.merged = null;"),
        "the same array comes back until a part is added, and adding one drops it",
    );
    c.that(
        CHARACTER.contains("if (planLanes.actions === actions) return planLanes.lanes;"),
        "which is what character.js's per-array lane cache needs, or it would redo every lane on every frame",
    );
    c.that(
        timeline.contains("actions: allActions(playing)") && timeline.contains("length: playing.end - playing.startedAt"),
        "the timeline carries all the parts' actions and ends where the last scheduled part does",
    );
}

/// `for_page` plans each voiced utterance on its own, so a turn in parts needs nothing new from it:
/// a part's plan is made from that part's text and audio alone, its times count from its own first
/// sample, and it does not depend on which parts were planned before it or what number it carries.
/// Run on the real function, not read from its text.
fn each_part_is_planned_from_its_own_text_and_audio(c: &mut Checks) {
    let b = figure_bindings();
    let part = |n: u64, text: &str, samples: &[f32]| {
        let mut event = voiced_event(text, samples);
        event["part"] = json!(n);
        event
    };
    let second_text = "[open]要不要先待在家吹冷氣？";
    let second_samples = clip(120, |f| if (4..100).contains(&f) { 6554 } else { 0 });
    let first = part(0, HAND_TEXT, &hand_samples());
    let second = part(1, second_text, &second_samples);

    let alone = crate::core::for_page(second.clone(), b);
    let first_planned = crate::core::for_page(first, b);
    let after_the_first = crate::core::for_page(second, b);
    c.eq(alone.get("plan"), after_the_first.get("plan"), "a part's plan is the same whether or not another part was planned first");
    c.eq(
        alone.get("plan"),
        Some(&json!(plan::plan(second_text, &second_samples, voice::SAMPLE_RATE, b))),
        "and it is the plan of that part's own text and audio",
    );
    c.eq(
        crate::core::for_page(part(0, second_text, &second_samples), b).get("plan"),
        alone.get("plan"),
        "whatever number the part carries",
    );
    let actions = alone.pointer("/plan/actions").and_then(Value::as_array).cloned().unwrap_or_default();
    c.that(!actions.is_empty(), "the part has actions, so the comparisons above are about something");
    c.that(
        alone.pointer("/plan/actions") != first_planned.pointer("/plan/actions"),
        "and they are its own and not the first part's",
    );
    let length = second_samples.len() as f64 / f64::from(voice::SAMPLE_RATE);
    c.that(
        actions.iter().all(|a| a["at"].as_f64().is_some_and(|at| (0.0..=length).contains(&at))),
        format!("every action lands inside the part, counted from its own first sample ({length} s long)"),
    );
}

// ---------------------------------------------------------------------------------------------
// pet_mic: hold the microphone button, speak, let go (Docs/DESIGN_STT.md 4.1.1, Docs/DESIGN_Voice.md
// 3.5). The lease and the wire are pure and checked as behaviour; the page and the registrations are
// checked as text, like the rest of `pet_page`, because a name that does not match arrives as nothing.

const STT: &str = include_str!("stt.rs");

/// The page from `start` to the first line that closes a top-level block after it, so a check looks
/// inside one function and does not match the same words somewhere else on the page.
/// The window has no title bar, taskbar entry or tray icon, so the quit button is the only way out
/// short of Task Manager. It has to be drawn, take clicks, ask before it acts, and reach a command
/// Rust registered and that really exits.
fn the_pet_can_be_quit_from_its_own_window(c: &mut Checks) {
    c.that(PAGE.contains("<button id=\"quit\""), "the page draws a quit button");
    c.that(
        page_block("function panelRegions(").contains("regionOf(quitEl)"),
        "and reports it as a region, or the click goes through to the desktop",
    );
    let handler = PAGE
        .find("quitEl.addEventListener(\"click\"")
        .map(|at| PAGE[at..].split("\n});").next().unwrap_or_default().to_string())
        .unwrap_or_default();
    let ask = handler.find("quitEl.textContent = \"確定結束？\"");
    let act = handler.find("invoke(\"quit\")");
    c.that(ask.is_some() && act.is_some(), "a press asks first and a second press invokes quit");
    c.that(handler.contains("if (quitAsking !== null)"), "and only a press made while it is asking quits");
    c.that(MAIN.contains("fn quit(") && MAIN.contains("app.exit(0)"), "Rust defines quit and it exits");
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    c.that(
        registered.split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|name| name == "quit"),
        "and registers it, behind the same one-window rule as every other command",
    );
}

/// Settings lists the characters bundled with the pet and she becomes the one chosen, after a
/// restart too. A folder dropped into `characters/` without a line in the index would be in the
/// executable and never offered, so the two are compared both ways.
fn she_is_chosen_from_the_bundled_characters(c: &mut Checks) {
    let Ok(index) = cast::Index::parse(CAST.as_bytes()) else {
        c.that(false, "characters/index.json parses");
        return;
    };
    let dir = ui_dir().join("characters");
    let mut folders: Vec<String> = std::fs::read_dir(&dir)
        .map(|it| it.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    folders.sort();
    let mut listed: Vec<String> = index.characters.iter().map(|id| id.to_string()).collect();
    listed.sort();
    c.eq(listed, folders, "the index lists every character folder and nothing else");

    let found = match cast::Found::load(&bundle_on_disk, None) {
        Ok(found) => found,
        Err(why) => {
            c.that(false, format!("the bundle loads: {why}"));
            return;
        }
    };
    c.eq(found.refused.clone(), Vec::new(), "every bundled character loads");
    for e in &found.characters {
        c.eq(e.source, cast::Source::Bundled, format!("{} is listed as bundled", e.id));
        c.eq(e.rig_url(), format!("characters/{}/rig.json", e.id), format!("{} is drawn from the bundle", e.id));
    }

    c.eq(found.pick(Some(index.default.as_str())).id.clone(), index.default.clone(), "a saved character that is bundled is the one shown");
    c.eq(found.pick(Some("gone")).id.clone(), index.default.clone(), "one the bundle no longer has falls back to the default");
    c.eq(found.pick(None).id.clone(), index.default.clone(), "and so does no choice at all");
    c.that(
        cast::Index::parse(br#"{"default":"b","characters":["a"]}"#).is_err(),
        "an index whose default is not in its list is refused",
    );
    c.that(
        cast::Index::parse(br#"{"default":"a","characters":[{"id":"a","name":"A"}]}"#).is_err(),
        "and so is one that still carries names, which live in character.json now",
    );

    let home = temp_home("cast");
    c.eq(settings::Store::open(Some(home.0.clone())).current().character, None, "a home with no settings has no choice");
    let _ = std::fs::write(home.0.join(settings::FILE), r#"{"voice":"mine"}"#);
    let (store, roster) = roster_at(&home.0);
    c.that(roster.choose(index.default.as_str(), &store).is_ok(), "the choice is saved");
    c.eq(settings::Store::open(Some(home.0.clone())).current().character, Some(index.default.clone()), "and read back");
    let kept = std::fs::read(home.0.join(settings::FILE)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    c.eq(kept.and_then(|v| v.get("voice").cloned()), Some(json!("mine")), "and every other setting is kept");

    c.that(page_block("function panelRegions(").contains("regionOf(settingsToggleEl)"), "the settings button takes clicks");
    c.that(PAGE.contains("invoke(\"open_settings\")"), "and opens the settings window");
    c.that(!PAGE.contains("id=\"settings\"") && !PAGE.contains("drawCast"), "the pet's window has no settings panel of its own");
    c.that(PAGE.contains("Character.mount(canvas, list.shown.rig)"), "the page draws the rig Rust names");
    c.that(
        source_block(SETTINGS_PAGE, "async function drawCast(").contains("invoke(\"choose_character\", { id: c.id })"),
        "the settings window chooses her",
    );
    let choose = source_block(MAIN, "fn choose_character(");
    let saved = choose.find("roster.choose(&id, &store)?;");
    let reloads = choose.find("tell_and_reload(&app, &core);");
    c.that(matches!((saved, reloads), (Some(s), Some(r)) if s < r), "and Rust saves the choice, then reloads the pet");
    c.that(source_block(MAIN, "fn tell_and_reload(").contains("win.reload()"), "which reloads the pet's window");
    c.that(!CHARACTER.contains("characters/"), "character.js names no character of its own");
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    let names: Vec<&str> = registered.split(|ch: char| !ch.is_alphanumeric() && ch != '_').collect();
    c.that(names.contains(&"characters") && names.contains(&"choose_character"), "Rust registers both commands");
}

fn page_block(start: &str) -> String {
    source_block(PAGE, start)
}

/// From `start` to the end of the top-level block it opens, in any of the files pinned as text.
fn source_block(source: &str, start: &str) -> String {
    let page = source.replace("\r\n", "\n");
    page.find(start)
        .map(|at| {
            let rest = &page[at..];
            let end = rest.find("\n}\n").map_or(rest.len(), |e| e + 2);
            rest[..end].to_string()
        })
        .unwrap_or_default()
}

fn the_lease_pins_the_window_while_held(c: &mut Checks) {
    let t0 = std::time::Instant::now();
    let held = Held::default();
    c.that(!held.pins_window(), "nothing held pins nothing");
    c.eq(press::ignore_cursor(held.pins_window(), Hit::Through), true, "and off every rectangle the window passes clicks through");

    c.that(held.take(t0), "a press takes the lease");
    c.that(!held.take(t0), "a second press while held is the same press");
    c.that(held.pins_window(), "a held lease pins the window");
    for hit in [Hit::Through, Hit::Character, Hit::Panel] {
        c.eq(press::ignore_cursor(held.pins_window(), hit), false, format!("so it takes the cursor even over {hit:?}"));
    }

    c.that(held.release(), "the release ends it");
    c.that(!held.release(), "and a second release is a no-op");
    c.eq(press::ignore_cursor(held.pins_window(), Hit::Through), true, "after which the window passes clicks through again");
    c.eq(press::ignore_cursor(held.pins_window(), Hit::Panel), false, "and still takes them over the dialogue");

    // A page that died holding the button cannot leave the window pinned for good.
    c.that(held.take(t0), "a new press");
    c.that(!held.expire_overdue(t0 + MAX_HOLD - std::time::Duration::from_millis(1)), "is not expired a moment early");
    c.that(held.expire_overdue(t0 + MAX_HOLD), "is expired at the limit");
    c.that(!held.pins_window(), "and pins nothing after");
    c.that(!held.expire_overdue(t0 + MAX_HOLD * 2), "and expiring again is a no-op");
    c.that(
        MAX_HOLD > std::time::Duration::from_secs(60),
        "the limit outlasts the service's own 60 s cap, so the service ends a stuck press first",
    );
}

fn the_poll_thread_asks_the_lease_first(c: &mut Checks) {
    c.that(
        MAIN.contains("press::ignore_cursor(held.pins_window(), cursor_hit(&handle, &win))"),
        "the poll thread decides click-through from the lease and the hit together",
    );
    c.that(!MAIN.contains("cursor_hit(&handle, &win) == hit::Hit::Through"), "and no longer from the hit alone");
    c.that(MAIN.contains("held.expire_overdue("), "it expires a lease that outlived its limit");
    c.that(MAIN.contains(".manage(press::Held::default())"), "and the lease is state the app manages");
}

/// The button has no rectangle of its own. It is one of the text row's children, and the row is the
/// one rectangle `Chat.regions` reports for the input root, so the hit test covers it exactly when
/// the row is drawn and never when the dialogue is closed.
fn the_button_lies_inside_the_text_row(c: &mut Checks) {
    c.that(PAGE.contains("Chat.mount(chatEls, {}, [micEl]);"), "the page hands the button to the dialogue's row");
    c.that(CHAT.contains("row.append(field, ...tools);"), "and chat.js puts it in the row, after the field");
    c.that(
        CHAT.contains("[roots.bubble, roots.choices, roots.input, roots.log].forEach"),
        "and the input root's children are reported as before",
    );

    // The pill as `regions()` reports it in a 460x620 window: 10 px in from each side, 8 px up, 40
    // tall, with the button at its right end.
    let row = px(10.0, 572.0, 440.0, 40.0);
    let regions = Regions { character: vec![character_rect()], panel: vec![row] };
    c.eq(over(&regions, 430.0, 592.0), Hit::Panel, "the button's place at the right of the row takes the click");
    c.eq(over(&regions, 455.0, 592.0), Hit::Through, "and right of the row the window passes it through");
}

fn a_press_and_every_kind_of_release_reach_rust(c: &mut Checks) {
    let down = page_block("function micDown(");
    c.that(down.contains("micEl.setPointerCapture(e.pointerId)"), "a press captures the pointer");
    c.that(down.contains("invoke(\"mic_press\")"), "and tells Rust");
    let up = page_block("function micUp(");
    c.that(up.contains("invoke(\"mic_release\")"), "a release tells Rust");
    c.that(
        up.contains("setMic(micOpen ? MicState.Recognising : MicState.Idle);"),
        "and waits for a result only when the service still has the press open",
    );
    let handler = page_block("function onSttEvent(");
    c.eq(handler.matches("micOpen = false;").count(), 3, "a transcript, a nothing and an error each close the press");
    c.that(
        PAGE.contains(
            "for (const type of [\"pointerup\", \"pointercancel\", \"lostpointercapture\"]) micEl.addEventListener(type, micUp);",
        ),
        "up, cancel and a lost capture are all the release",
    );
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    for command in ["mic_press", "mic_release", "stt_state"] {
        c.that(MAIN.contains(&format!("fn {command}(")), format!("Rust defines {command}"));
        let listed = registered.split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|name| name == command);
        c.that(listed, format!("and registers {command}"));
    }
    c.that(MAIN.contains(".manage(stt::Stt::default())") && MAIN.contains("stt::start("), "Rust manages and starts the service");
    c.eq((stt::EVENT, stt::STATE_EVENT), ("stt-event", "stt-state"), "the two event names");
    c.that(
        PAGE.contains("listen(\"stt-event\"") && PAGE.contains("listen(\"stt-state\""),
        "are the ones the page listens for",
    );
    c.that(PAGE.contains("invoke(\"stt_state\")"), "and the page asks the state once");
}

/// The service's own words, written out as literal lines the way `pet_core` writes the core's. If
/// Stt/host/src/proto.rs changes a key, this is what turns red.
fn the_service_wire_is_restated_to_the_letter(c: &mut Checks) {
    c.eq(stt::command_line(Op::Press), "{\"op\":\"press\"}\n".to_string(), "a press is one line");
    c.eq(stt::command_line(Op::Release), "{\"op\":\"release\"}\n".to_string(), "a release is one line");

    let line = |bytes: &[u8]| parse_line(bytes).unwrap_or(Value::Null);
    let ready = line(br#"{"ev":"ready","version":1,"model":"sense-voice","vad":"silero_vad","load_ms":900,"capture":{"device":"Mic (HD)","native_rate":48000,"native_channels":2,"resampled":true}}"#);
    c.eq(
        SttState::Starting.after(&ready),
        Some(SttState::Ready { model: "sense-voice".into(), device: Some("Mic (HD)".into()), resampled: Some(true), device_error: None }),
        "ready with a device",
    );
    let nodev = line(br#"{"ev":"ready","version":1,"model":"m","vad":"v","load_ms":1,"capture":null,"capture_error":"no default input device"}"#);
    c.eq(
        SttState::Starting.after(&nodev),
        Some(SttState::Ready { model: "m".into(), device: None, resampled: None, device_error: Some("no default input device".into()) }),
        "ready without a device keeps the reason",
    );
    c.eq(SttState::Down { why: "x".into() }.after(&ready), None, "down is final");
    c.eq(SttState::Starting.after(&line(br#"{"ev":"capture_started","session":1}"#)), None, "only ready moves the state");

    let fatal = line(br#"{"ev":"error","session":null,"kind":"model","message":"model file missing: C:\\x"}"#);
    let why = stt::fatal_why(&fatal).unwrap_or_default();
    c.that(why.contains("model file missing"), format!("a model failure is kept in its own words: {why}"));
    c.eq(stt::exit_why(Some(3), Some(why.clone())), why, "and is what the exit says");
    c.eq(stt::fatal_why(&line(br#"{"ev":"error","session":2,"kind":"device","message":"busy"}"#)), None, "a device failure is not fatal");

    // The page reads the state under the names serde writes.
    let v = serde_json::to_value(SttState::Ready { model: "m".into(), device: None, resampled: None, device_error: Some("e".into()) })
        .unwrap_or(Value::Null);
    c.eq(v.get("state").and_then(Value::as_str), Some("ready"), "Ready is \"ready\"");
    c.that(v.get("deviceError").is_some(), format!("the device's reason is deviceError: {v}"));
    c.that(PAGE.contains("s.state === \"ready\"") && PAGE.contains("s.deviceError"), "which is what the page reads");
    let d = serde_json::to_value(SttState::Down { why: "w".into() }).unwrap_or(Value::Null);
    c.eq(d, json!({ "state": "down", "why": "w" }), "Down is \"down\" with why");
    c.that(PAGE.contains("s.state === \"down\"") && PAGE.contains("String(s.why)"), "which the page reads too");
    let handler = page_block("function onSttEvent(");
    for read in [
        "p.ev === \"capture_started\"",
        "p.ev === \"transcript\"",
        "p.ev === \"nothing\"",
        "p.ev === \"error\"",
        "p.session",
        "p.seq",
        "p.text",
        "p.why",
        "p.kind",
        "p.message",
    ] {
        c.that(handler.contains(read), format!("the page's handler reads {read}"));
    }
}

fn the_service_is_looked_for_beside_the_core(c: &mut Checks) {
    let exe = format!("vrstt{}", std::env::consts::EXE_SUFFIX);
    let root = Path::new(r"R:\repo");
    let dir = root.join("Pet").join("src-tauri").join("target").join("debug");
    let built = |profile: &str| root.join("Stt").join("target").join(profile).join(&exe);
    c.eq(
        stt::candidates(None, Some(&dir), false),
        vec![dir.join(&exe), built("debug"), built("release")],
        "beside the pet, then the Stt workspace's builds",
    );
    let mine = OsString::from(r"X:\stt.exe");
    c.eq(stt::candidates(Some(mine.clone()), Some(&dir), false), vec![PathBuf::from(&mine)], "a named path is the only place looked");
    let why = stt::locate(&[built("debug")], |_| false).err().unwrap_or_default();
    c.that(why.contains(stt::EXE_ENV) && why.contains("vrstt"), format!("a missing service says where it looked and how to point at it: {why}"));
    let core_exe = format!("vrchatcore{}", std::env::consts::EXE_SUFFIX);
    c.eq(
        candidates(None, Some(&dir), false).get(1).cloned(),
        Some(root.join("ChatCore").join("target").join("debug").join(core_exe)),
        "the core's search shares the function and still looks where it did",
    );
}

fn the_character_stops_before_the_microphone_opens(c: &mut Checks) {
    let handler = page_block("function onSttEvent(");
    let started = handler.find("p.ev === \"capture_started\"");
    let stop = handler.find("Speaker.stop();");
    let next = handler.find("p.ev === \"transcript\"");
    c.that(
        matches!((started, stop, next), (Some(a), Some(b), Some(n)) if a < b && b < n),
        format!("capture_started stops the speaker, inside its own branch: {started:?} {stop:?} {next:?}"),
    );
}

fn a_transcript_takes_the_typed_path_once(c: &mut Checks) {
    let chat = CHAT.replace("\r\n", "\n");
    c.that(chat.contains("submit: send,"), "Chat.submit is the function the field calls");
    c.that(chat.contains("field.value = \"\";\n      send(text);"), "and the field calls send");
    let handler = page_block("function onSttEvent(");
    let seen = handler.find("!micSeen.has(key)");
    let add = handler.find("micSeen.add(key);");
    let submit = handler.find("Chat.submit(p.text);");
    c.that(
        matches!((seen, add, submit), (Some(a), Some(b), Some(s)) if a < b && b < s),
        format!("a transcript is checked against the ones already sent, recorded, then submitted: {seen:?} {add:?} {submit:?}"),
    );
    c.that(handler.contains("const key = p.session + \":\" + p.seq;"), "and the key is the session and the sequence");
    c.eq(PAGE.matches("Chat.submit(").count(), 1, "nothing else on the page submits a sentence for the user");
}

// ---------------------------------------------------------------------------------------------
// pet_characters: profiles, the user's folders, and the scheme that serves them
// (Docs/DESIGN_Characters.md).

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui")
}

/// The bundle as the asset resolver would answer, read from `frontendDist` on disk, which is the
/// folder the build compiles in.
fn bundle_on_disk(path: &str) -> Option<Vec<u8>> {
    std::fs::read(ui_dir().join(path)).ok()
}

fn profile_of(value: Value) -> Result<cast::Profile, String> {
    cast::Profile::parse(&serde_json::to_vec(&value).unwrap_or_default())
}

/// `outfit` tells two folders of one character apart (使用者 2026-11-08: 小克 and 小克 in a gown). It
/// keeps to its limits, it is shown in the settings list beside her name and nowhere else, and the
/// core is never told it, since she is the same person in either.
fn an_outfit_names_her_look_in_the_list_only(c: &mut Checks) {
    let with = |outfit: Value| profile_of(json!({ "name": "小克", "persona": "p", "outfit": outfit }));
    c.eq(with(json!("禮服")).map(|p| p.outfit), Ok(Some("禮服".to_string())), "an outfit is read");
    c.that(with(json!("禮".repeat(16))).is_ok(), "16 characters is the most, counted as characters");
    c.eq(profile_of(json!({ "name": "小克", "persona": "p" })).map(|p| p.outfit), Ok(None), "and it is optional");
    for (what, value) in [
        ("17 characters", json!("禮".repeat(17))),
        ("an empty one", json!("")),
        ("one of spaces", json!("   ")),
        ("a newline", json!("禮\n服")),
        ("a bracket", json!("[gown]")),
        ("a number", json!(3)),
    ] {
        let parsed = with(value);
        c.that(parsed.as_ref().is_err_and(|w| w.contains("outfit") || w.contains("invalid type")), format!("{what} is refused: {parsed:?}"));
    }
    c.that(
        profile_of(json!({ "name": "小克", "persona": "p", "outfit": "禮服", "outift": "x" })).is_err_and(|w| w.contains("outift")),
        "a misspelt field beside it is still refused",
    );

    let gown = cast::Character {
        id: cast::Id::parse("gown").expect("a valid id"),
        source: cast::Source::User,
        profile: with(json!("禮服")).expect("a profile with an outfit"),
    };
    let listed = gown.listed();
    c.eq((listed.name.as_str(), listed.outfit.as_deref()), ("小克", Some("禮服")), "the list is given her name and the outfit apart");
    let label = source_block(SETTINGS_PAGE, "function labelOf(");
    c.that(label.contains("return c.outfit ? c.name + \"（\" + c.outfit + \"）\" : c.name;"), "and the settings list shows 小克（禮服）, or the name alone");
    c.that(source_block(SETTINGS_PAGE, "async function drawCast(").contains("labelOf(c)"), "for each character");
    c.that(PAGE.contains("Chat.rename({ character: list.shown.name });") && !PAGE.contains("outfit") && !CHAT.contains("outfit"), "the dialogue's name tag is her name alone");
    let line = crate::core::character_line(&cast::Speaking { profile: gown.profile.clone(), voice: None });
    c.that(!line.contains("outfit") && !line.contains("禮服"), format!("and the core is not told the outfit: {line}"));

    let home = make_home("outfit");
    let (_, roster) = roster_at(&home.root);
    let edit = |outfit: &str| cast::Edit { name: "阿好".into(), outfit: outfit.into(), persona: "好人。".into(), greeting: String::new(), fillers: vec![] };
    let file = home.chars.join("good").join(cast::PROFILE);
    let read = || cast::Profile::parse(&std::fs::read(&file).unwrap_or_default()).ok().and_then(|p| p.outfit);
    c.that(roster.save_profile("good", edit("便服")).is_ok(), "the editor saves a user character's outfit");
    c.eq(read(), Some("便服".to_string()), "and it is written");
    c.that(roster.save_profile("good", edit("[x]")).is_err(), "one the scan would refuse is not");
    c.eq(read(), Some("便服".to_string()), "and the file keeps the last good one");
    c.that(roster.save_profile("good", edit("")).is_ok() && read().is_none(), "an empty field is no outfit");
    c.that(SETTINGS_PAGE.contains("id=\"profileOutfit\"") && source_block(SETTINGS_PAGE, "async function saveProfile(").contains("outfit: document.getElementById(\"profileOutfit\").value.trim(),"), "the editor has the field and sends it");
}

fn a_profile_keeps_to_its_limits(c: &mut Checks) {
    let full = json!({
        "name": "小毛", "persona": "你住在使用者的桌面角落。", "greeting": "嗨。",
        "voice": "voice.wav", "fillers": ["嗯……", "我想想喔"],
    });
    c.that(profile_of(full).is_ok(), "a profile with every field parses");
    c.that(profile_of(json!({ "name": "a", "persona": "b" })).is_ok(), "and one with only the two it needs");

    let with = |key: &str, value: Value| {
        let mut p = json!({ "name": "小毛", "persona": "p" });
        p[key] = value;
        profile_of(p)
    };
    // Characters and not bytes: 32 of these is 96 bytes.
    c.that(with("name", json!("字".repeat(32))).is_ok(), "a name of 32 characters is allowed");
    c.that(with("name", json!("字".repeat(33))).is_err_and(|w| w.contains("33")), "33 is refused, and the reason counts them");
    c.that(with("name", json!("  ")).is_err(), "a blank name is refused");
    c.that(with("persona", json!("字".repeat(2000))).is_ok(), "a persona of 2000 characters is allowed");
    c.that(with("persona", json!("字".repeat(2001))).is_err(), "2001 is refused");
    c.that(with("greeting", json!("字".repeat(200))).is_ok(), "a greeting of 200 is allowed");
    c.that(with("greeting", json!("字".repeat(201))).is_err(), "201 is refused");
    // Stricter than the design's table, to match what the core refuses (cast.rs, `Profile::check`).
    c.that(with("greeting", json!(" \t")).is_err(), "a greeting that is present and blank is refused");
    c.that(with("greeting", json!("")).is_err(), "and so is an empty one");
    c.that(with("greeting", json!("嗨[nod]")).is_err_and(|w| w.contains("方括號")), "a greeting with a square bracket is refused");
    c.that(with("greeting", json!("嗨]")).is_err(), "either bracket");
    c.that(with("name", json!("小\n毛")).is_err_and(|w| w.contains("控制字元")), "a name with a newline is refused");
    c.that(with("name", json!("小\u{7}毛")).is_err(), "and with any other control character");
    c.that(with("persona", json!(" \n ")).is_err(), "a blank persona is refused");
    c.that(with("fillers", json!(vec!["嗯"; 12])).is_ok(), "12 fillers are allowed");
    c.that(with("fillers", json!(vec!["嗯"; 13])).is_err(), "13 are refused");
    c.that(with("fillers", json!(["字".repeat(40)])).is_ok(), "a filler of 40 characters is allowed");
    c.that(with("fillers", json!(["字".repeat(41)])).is_err(), "41 is refused");
    c.that(
        with("voice", json!("me.wav")).is_err_and(|w| w.contains("voice.wav")),
        "a voice under another name is refused, saying the one it must be",
    );

    c.that(with("nmae", json!("小毛")).is_err_and(|w| w.contains("nmae")), "a misspelt field is refused and the reason names it");
    c.that(profile_of(json!({ "name": "小毛" })).is_err_and(|w| w.contains("persona")), "a missing persona is refused by name");
    c.that(cast::Profile::parse(b"{ not json").is_err(), "and something that is not JSON is refused");
}

/// The persona says who she is. The rules every character keeps (she hears and speaks, clauses and
/// gestures, no square brackets) stay in the core where a profile cannot reach them, so a copy of
/// them here would be a second place to change and the one that does not count.
fn the_bundled_profile_says_who_she_is_and_no_rules(c: &mut Checks) {
    let Some(bytes) = bundle_on_disk("characters/char-20261107/character.json") else {
        c.that(false, "the bundled character has a character.json");
        return;
    };
    match cast::Profile::parse(&bytes) {
        Err(why) => c.that(false, format!("it parses: {why}")),
        Ok(p) => {
            c.eq(p.name.as_str(), "小克", "she is 小克");
            c.eq(p.greeting.as_deref(), Some("嗨，我在這裡。"), "and opens with her greeting");
            c.that(p.persona.contains("桌面角落"), "the persona is who she is");
            // The core opens the prompt with 「你是一個桌面寵物角色，名字叫{name}。」 itself
            // (ChatCore/src/reply.rs), so the sentence here would be said twice, and a persona that
            // differs from the core's own default restarts claude at every start.
            c.that(!p.persona.contains("名字叫"), "and does not repeat the sentence the core adds from her name");
            for rule in ["手勢", "方括號", "麥克風", "子句"] {
                c.that(!p.persona.contains(rule), format!("and carries none of the core's fixed rules ({rule})"));
            }
            c.eq(p.voice, None, "she speaks in the default voice");
        }
    }
}

fn an_id_follows_the_folder_rule(c: &mut Checks) {
    let longest = "a".repeat(48);
    for good in ["a", "0", "char-20261107", "my_pet-2", longest.as_str()] {
        c.that(cast::Id::parse(good).is_ok(), format!("{good:?} is an id"));
    }
    let too_long = "a".repeat(49);
    for bad in ["", "A", "-a", "_a", "a b", "a.b", "..", "a/b", "a\\b", "小毛", "a:b", too_long.as_str()] {
        c.that(cast::Id::parse(bad).is_err(), format!("{bad:?} is not"));
    }
}

/// A temporary home with one folder of each kind the design names. Removed when dropped.
struct Home {
    root: PathBuf,
    chars: PathBuf,
    /// Why a link could not be made, when the OS refused to make it.
    skipped: Vec<String>,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The settings and the roster at `home`, made the way the app makes them when it starts: the roster
/// shows the choice the settings hold, and saves a new one through them.
fn roster_at(home: &Path) -> (settings::Store, cast::Roster) {
    let store = settings::Store::open(Some(home.to_path_buf()));
    let roster = cast::Roster::load(&bundle_on_disk, Some(home.to_path_buf()), &store);
    (store, roster)
}

fn put(dir: &Path, file: &str, bytes: &[u8]) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(file), bytes);
}

/// A file of `len` bytes with none of them written: the checks read the length, and the disk
/// is not asked for 200 MB of zeros.
fn sized(path: &Path, len: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::File::create(path).and_then(|f| f.set_len(len));
}

fn character_folder(dir: &Path, profile: Value) {
    put(dir, "character.json", &serde_json::to_vec(&profile).unwrap_or_default());
    put(dir, "rig.json", RIG.as_bytes());
    put(dir, "layers.json", ART.as_bytes());
}

fn make_home(tag: &str) -> Home {
    let root = std::env::temp_dir().join(format!("vrpet-chars-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let chars = root.join("characters");
    let mut skipped = Vec::new();
    let mib = 1024 * 1024;
    let plain = || json!({ "name": "x", "persona": "y" });

    let good = chars.join("good");
    character_folder(&good, json!({ "name": "阿好", "persona": "好人。", "greeting": "你好。", "voice": "voice.wav" }));
    put(&good, "voice.wav", b"RIFF");
    put(&good, "a.png", b"\x89PNG");
    put(&good, "my arm.png", b"\x89PNG");
    put(&good, "notes.txt", b"not served");
    let _ = std::fs::create_dir_all(good.join("sub.png"));

    character_folder(&chars.join("Bad Name"), plain());
    character_folder(&chars.join("big"), plain());
    sized(&chars.join("big").join("a.png"), 16 * mib + 1);
    character_folder(&chars.join("huge"), plain());
    sized(&chars.join("huge").join("blob.bin"), 200 * mib + 1);
    character_folder(&chars.join("norig"), plain());
    let _ = std::fs::remove_file(chars.join("norig").join("rig.json"));
    character_folder(&chars.join("char-20261107"), json!({ "name": "冒牌", "persona": "y" }));
    character_folder(&chars.join("noisy"), json!({ "name": "x", "persona": "y", "nmae": "z" }));
    character_folder(&chars.join("mute"), json!({ "name": "x", "persona": "y", "voice": "voice.wav" }));
    put(&chars, "README.txt", b"a file beside the folders is not a character");

    #[cfg(windows)]
    {
        // A junction needs no privilege, so this one is made on any Windows.
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(chars.join("linked")).arg(&good).output();
        match made {
            Ok(out) if out.status.success() => {}
            Ok(out) => skipped.push(format!("junction: {}", String::from_utf8_lossy(&out.stdout).trim())),
            Err(e) => skipped.push(format!("junction: {e}")),
        }
        // A file symlink needs Developer Mode or an elevated process, so the OS may refuse it.
        let linkfile = chars.join("linkfile");
        character_folder(&linkfile, plain());
        let _ = std::fs::remove_file(linkfile.join("rig.json"));
        if let Err(e) = std::os::windows::fs::symlink_file(good.join("rig.json"), linkfile.join("rig.json")) {
            skipped.push(format!("file symlink: {e}"));
            let _ = std::fs::remove_dir_all(&linkfile);
        }
    }
    Home { root, chars, skipped }
}

fn every_user_folder_is_loaded_or_says_why(c: &mut Checks) {
    let home = make_home("scan");
    for why in &home.skipped {
        eprintln!("pet_characters: a link check is skipped because the OS refused to make the link: {why}");
    }
    let bundled = [cast::Id::parse("char-20261107").expect("a valid id")];
    let (found, refused) = cast::scan(&home.chars, &bundled);

    let ids: Vec<&str> = found.iter().map(|c| c.id.as_str()).collect();
    c.eq(ids, vec!["good"], "the one good folder is the one loaded");
    if let Some(good) = found.first() {
        c.eq(good.source, cast::Source::User, "and it is the user's");
        c.eq(good.profile.name.as_str(), "阿好", "with its own name");
        c.eq(good.rig_url(), format!("{}good/rig.json", cast::ORIGIN), "and drawn through the scheme");
    }

    let why = |id: &str| refused.iter().find(|r| r.id == id).map(|r| r.why.clone());
    let mut expect = vec![
        ("Bad Name", "不合規則"),
        ("big", "超過 16 MB"),
        ("huge", "整個角色"),
        ("norig", "缺 rig.json"),
        ("char-20261107", "內建"),
        ("noisy", "nmae"),
        ("mute", "voice.wav"),
    ];
    if !home.skipped.iter().any(|s| s.starts_with("junction")) {
        expect.push(("linked", "捷徑"));
    }
    if !home.skipped.iter().any(|s| s.starts_with("file symlink")) {
        expect.push(("linkfile", "捷徑"));
    }
    for (id, part) in &expect {
        match why(id) {
            Some(w) => c.that(w.contains(part), format!("{id} is listed and says why ({part}): {w}")),
            None => c.that(false, format!("{id} is listed as refused")),
        }
    }
    c.eq(refused.len(), expect.len(), "and nothing else is listed, not even the file beside them");
    c.that(refused.iter().all(|r| r.source == cast::Source::User), "every refusal says it is the user's folder");
    c.eq(cast::scan(&home.chars.join("absent"), &bundled).0.len(), 0, "a home with no characters folder has none");
}

fn a_chosen_user_character_survives_a_restart_and_falls_back(c: &mut Checks) {
    let home = make_home("choice");
    let (store, roster) = roster_at(&home.root);
    c.that(roster.found().is_ok(), "the roster loads with user folders beside the bundle");
    c.eq(roster.shown().map(|s| s.id.to_string()), Some("char-20261107".to_string()), "with no choice she is the default");
    c.that(roster.choose("big", &store).is_err(), "a refused folder cannot be chosen");
    c.that(roster.choose("nobody", &store).is_err(), "nor one that is not there");
    c.that(roster.choose("good", &store).is_ok(), "a user character can");

    let (_, again) = roster_at(&home.root);
    let shown = again.shown();
    c.eq(shown.as_ref().map(|s| s.id.to_string()), Some("good".to_string()), "and she is still that one after a restart");
    c.eq(shown.map(|s| s.source), Some(cast::Source::User), "drawn from the user's folder");

    let _ = std::fs::remove_file(home.chars.join("good").join("rig.json"));
    let (_, broken) = roster_at(&home.root);
    c.eq(
        broken.shown().map(|s| s.id.to_string()),
        Some("char-20261107".to_string()),
        "once her folder breaks she falls back to the default",
    );
    let listed = broken.found().ok().and_then(|f| f.refused.iter().find(|r| r.id == "good").cloned());
    c.that(listed.is_some_and(|r| r.why.contains("rig.json")), "and the broken folder is listed with why");
}

fn the_scheme_serves_only_checked_files(c: &mut Checks) {
    let home = make_home("scheme");
    let (_, roster) = roster_at(&home.root);
    let served = |path: &str| roster.serve(path);

    match served("/good/rig.json") {
        Ok((bytes, kind)) => {
            c.eq(kind, cast::Kind::Json, "rig.json is served as JSON");
            c.that(bytes == RIG.as_bytes(), "and is the file's bytes");
        }
        Err(r) => c.that(false, format!("/good/rig.json is served: {r:?}")),
    }
    c.that(served("/good/my%20arm.png").is_ok_and(|(_, k)| k == cast::Kind::Png), "an escaped space is the file's own name");
    c.that(served("/good/voice.wav").is_ok_and(|(_, k)| k == cast::Kind::Wav), "a wav is served");

    // The URL the page is handed for her rig is one this serves.
    let url = roster.found().ok().and_then(|f| f.find("good").map(cast::Character::rig_url)).unwrap_or_default();
    let path = url.strip_prefix(cast::ORIGIN).map(|p| format!("/{p}")).unwrap_or_default();
    c.that(served(&path).is_ok(), format!("the rig URL the page gets ({url}) is one the scheme serves"));

    let forbidden = [
        "/good/../norig/rig.json",
        "/good/..%2Fnorig%2Frig.json",
        "/good/..%5Crig.json",
        "/good/a%5Cb.png",
        "/good/C:%5CWindows%5Cwin.ini",
        "/good/rig.json:x.png",
        "/good/notes.txt",
        "/good/%ZZ.png",
        "/good/a%00.png",
        "/good/",
        "good/rig.json",
    ];
    for path in forbidden {
        match served(path) {
            Err(cast::Refusal::Forbidden(_)) => c.that(true, ""),
            other => c.that(false, format!("{path} is refused as forbidden, saw {other:?}")),
        }
    }
    let unknown = ["/nobody/rig.json", "/char-20261107/rig.json", "/big/rig.json", "/Bad%20Name/rig.json", "//good/rig.json", "/good/gone.png"];
    for path in unknown {
        match served(path) {
            Err(cast::Refusal::Missing(_)) => c.that(true, ""),
            other => c.that(false, format!("{path} is not found, saw {other:?}")),
        }
    }
    // Windows will not open a folder as a file at all, and elsewhere it opens and is not a regular
    // file, so the status differs and only the refusal is the point.
    c.that(served("/good/sub.png").is_err(), "a folder named like a PNG is not served");
    c.eq(cast::Refusal::Missing(String::new()).status(), 404, "a missing file is a 404");
    c.eq(cast::Refusal::Forbidden(String::new()).status(), 403, "a refused one is a 403");

    // The scan's word is not taken for it: a file that grew since is refused when it is asked for.
    sized(&home.chars.join("good").join("a.png"), 16 * 1024 * 1024 + 1);
    c.that(
        matches!(served("/good/a.png"), Err(cast::Refusal::Forbidden(ref w)) if w.contains("超過")),
        "a PNG over its cap after the scan is refused when it is asked for",
    );

    c.that(cast::ORIGIN.contains(cast::SCHEME), "the origin the page uses is the scheme Rust registers");
    c.that(MAIN.contains("register_asynchronous_uri_scheme_protocol(cast::SCHEME"), "Rust registers it");
    c.that(
        MAIN.contains("refuse(label, scheme)") && MAIN.contains("user_file(&label, cast::SCHEME, &request, serve)"),
        "behind the one-window rule",
    );
    c.that(
        MAIN.contains(".header(\"Access-Control-Allow-Origin\", \"*\")"),
        "and answers with the header that keeps the canvas readable",
    );
}

fn the_core_can_be_told_who_she_is(c: &mut Checks) {
    let home = make_home("speaking");
    let (store, roster) = roster_at(&home.root);
    let bundled = roster.speaking();
    c.eq(bundled.as_ref().map(|s| s.profile.name.as_str()), Some("小克"), "the default is 小克");
    c.eq(bundled.and_then(|s| s.voice), None, "in the default voice");
    c.that(roster.choose("good", &store).is_ok(), "a user character with a voice is chosen");
    let user = roster.speaking();
    c.eq(user.as_ref().map(|s| s.profile.name.as_str()), Some("阿好"), "the core would get her profile");
    c.eq(user.and_then(|s| s.voice).map(|v| v.to_string()), Some("good".to_string()), "and her id as the voice to clone");
}

fn the_page_draws_and_names_what_rust_gives_it(c: &mut Checks) {
    c.that(PAGE.contains("Character.mount(canvas, list.shown.rig)"), "the figure is the rig Rust names");
    c.that(PAGE.contains("Chat.rename({ character: list.shown.name });"), "the dialogue's name is hers");
    c.that(!PAGE.contains("character: \"小克\""), "and is not written into the page");
    c.that(
        PAGE.contains("text: list.shown.greeting || OPENING"),
        "the bubble opens with her greeting, and the page's own line only when she has none",
    );
    c.that(CHAT.contains("rename: function (labels)"), "chat.js can rename after mounting");

    let cast_block = source_block(SETTINGS_PAGE, "async function drawCast(");
    c.that(cast_block.contains("...list.refused.map("), "the settings list shows the folders that were refused");
    c.that(cast_block.contains("el(\"div\", \"cast-item off\", r.id + \"：\" + r.why)"), "greyed, with the reason, as text");
    c.that(SETTINGS_PAGE.contains("n.textContent = text"), "which el() sets as textContent");

    let load = CHARACTER.find("function loadImage(").map(|at| &CHARACTER[at..]).unwrap_or("");
    let cors = load.find("img.crossOrigin = \"anonymous\";");
    let src = load.find("img.src = path;");
    c.that(cors.is_some() && cors < src, "an image is asked for with CORS before its source is set, so the canvas stays readable");
}

/// The core is told who she is by the line built from whoever the roster shows, as each core
/// starts and as she is chosen. Starting is where a core that was down when she was chosen learns
/// it, so nothing has to be kept to send later.
fn the_core_hears_who_she_is_before_anything_else(c: &mut Checks) {
    let home = make_home("tell");
    let (store, roster) = roster_at(&home.root);
    let told = |roster: &cast::Roster| {
        roster.speaking().map(|s| serde_json::from_str::<Value>(&crate::core::character_line(&s)).unwrap_or(Value::Null))
    };
    let first = told(&roster);
    c.eq(first.as_ref().and_then(|l| l.get("name")).and_then(Value::as_str), Some("小克"), "a core that starts with nothing chosen is told she is 小克");
    c.that(first.as_ref().is_some_and(|l| l.get("voice").is_none()), "in the default voice");
    c.that(roster.choose("good", &store).is_ok(), "a user character is chosen");
    let then = told(&roster);
    c.eq(then.as_ref().and_then(|l| l.get("name")).and_then(Value::as_str), Some("阿好"), "and the line is hers from then on");
    c.eq(then.as_ref().and_then(|l| l.get("voice")).and_then(Value::as_str), Some("good"), "with her own voice");
    let (_, restarted) = roster_at(&home.root);
    c.eq(told(&restarted), then, "and a core started later, by a pet started later, is told the same");

    let launch = source_block(CORE, "fn launch(");
    let tell = launch.find("character_line(&speaking)");
    let shared = launch.find(".piped.adopt(stdin, child)");
    c.that(
        matches!((tell, shared), (Some(t), Some(s)) if t < s),
        format!("a core is told who she is before its pipe is shared, so no say or greeting goes first: {tell:?} then {shared:?}"),
    );
    c.that(launch.contains("app.state::<cast::Roster>().speaking()"), "and told whoever is shown as it starts");
    let choose = source_block(MAIN, "fn choose_character(");
    let saved = choose.find("roster.choose(&id, &store)?;");
    let tells = choose.find("tell_and_reload(&app, &core);");
    c.that(matches!((saved, tells), (Some(s), Some(t)) if s < t), "a choice is saved, then the running core is told");
    let tell = source_block(MAIN, "fn tell_and_reload(");
    c.that(tell.contains("core.tell(&speaking)") && !tell.contains("core.tell(&speaking)?"), "and a core that cannot be told does not undo the choice");
}

/// Her greeting is in the bubble as text from the moment the page has it, so a core that is down
/// leaves her saying something. The core voices it a moment later, and that utterance plays and
/// moves her mouth like an answer while the bubble keeps the one copy. Pinned as text, like the rest
/// of the page, because this crate cannot run it; the live check is what runs it.
fn her_greeting_is_voiced_once_and_shown_once(c: &mut Checks) {
    let greet = source_block(MAIN, "async fn greet(");
    c.that(greet.contains("c.profile.greeting"), "Rust voices her profile's greeting and takes no text from the page");
    c.that(greet.contains("return Ok(None);"), "a profile with no greeting sends nothing");
    c.that(greet.contains("core.greet(&text)?;"), "and the line goes to the core");
    let registered: String =
        MAIN.find("generate_handler![").map(|at| MAIN[at..].chars().take_while(|&ch| ch != ']').collect()).unwrap_or_default();
    c.that(registered.split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|n| n == "greet"), "greet is a command");

    c.that(PAGE.contains("text: list.shown.greeting || OPENING"), "the bubble opens with her greeting as text");
    c.eq(PAGE.matches("invoke(\"greet\")").count(), 1, "the page asks for it in one place");
    let listen = PAGE.find(&format!("listen(\"{CORE_EVENT}\""));
    let asks = PAGE.find(".then(askGreeting)");
    c.that(matches!((listen, asks), (Some(l), Some(a)) if l < a), "and only after it listens for the answer");
    c.that(page_block("function askGreeting(").contains("{ words: spoken(text), answered: false }"), "it keeps the words as the bubble compares them");

    let body = page_block("function onCoreEvent(");
    c.that(
        body.contains("const greets = !later && greeting !== null && greeting.words === words;"),
        "the utterance that brings her greeting back is told apart by its words",
    );
    let branch = body.find("} else if (greets) {").map(|at| &body[at..]).unwrap_or("");
    let branch = branch.find("} else {").map_or(branch, |end| &branch[..end]);
    c.that(branch.contains("Chat.append(text.slice(words.length))"), "it adds to the bubble only what follows her words");
    c.that(!branch.contains("Chat.show("), "and opens no second bubble");
    c.eq(body.matches("Chat.show(").count(), 1, "the one bubble the handler opens is an answer's");
    let speak = body.find("Speaker.speak(");
    let dropped = body.find("greeted = \"dropped\";");
    c.that(matches!((dropped, speak), (Some(d), Some(s)) if d < s), "its voice takes the answer's path unless it was dropped first");
    c.that(PAGE.contains("if (greeting) greeting.answered = true;"), "and it is dropped once the user has said something");
}

/// Her fillers come out of her own folder by the same checked read the scheme uses, and the rule
/// is applied to what that read finds.
fn a_users_fillers_come_from_her_own_folder(c: &mut Checks) {
    let home = make_home("fillers");
    let folder = home.chars.join("good");
    put(&folder, cast::FILLERS, &bank_bytes(json!([bank_filler("line_0", 30), bank_filler("line_1", 40)])));
    let (_, roster) = roster_at(&home.root);
    let character = |id: &str| roster.found().ok().and_then(|f| f.find(id).cloned());
    let bank_of = |c: &cast::Character| voice::Bank::of(roster.own_file(c, cast::FILLERS, &bundle_on_disk), c.profile.voice.is_some());
    let (Some(good), Some(bundled)) = (character("good"), character("char-20261107")) else {
        c.that(false, "the user character and the bundled one both load");
        return;
    };

    let (bank, why) = bank_of(&good);
    c.that(matches!(&bank, voice::Bank::Own(f) if f.len() == 2), format!("a user character with her own bank plays it: {bank}"));
    c.eq(why, None, "and there is nothing to log");
    let (bank, _) = bank_of(&bundled);
    c.that(matches!(bank, voice::Bank::Default), format!("小毛 has neither a bank nor a voice and plays the default: {bank}"));

    let _ = std::fs::remove_file(folder.join(cast::FILLERS));
    let (bank, why) = bank_of(&good);
    c.that(matches!(bank, voice::Bank::Quiet), format!("with her bank gone and her own voice she plays none: {bank}"));
    c.eq(why, None, "and a bank she does not have is not logged");

    // A bank holds audio, so it may be past the 1 MiB a rig gets (cast.rs, `Kind::cap`). Over its
    // own 8 MiB, grown after the scan the way the scheme test grows a PNG, it is not read.
    sized(&folder.join(cast::FILLERS), 2 * 1024 * 1024);
    let (_, why) = bank_of(&good);
    c.that(
        why.as_deref().is_some_and(|w| !w.contains("超過")),
        format!("a 2 MiB bank is read, and refused only for what is in it: {why:?}"),
    );
    sized(&folder.join(cast::FILLERS), 8 * 1024 * 1024 + 1);
    let (bank, why) = bank_of(&good);
    c.that(matches!(bank, voice::Bank::Quiet), "a bank over 8 MiB is not read");
    c.that(why.as_deref().is_some_and(|w| w.contains("超過")), format!("and the read's refusal is what is logged: {why:?}"));

    c.that(MAIN.contains("roster.own_file(&shown, cast::FILLERS"), "the filler command reads the shown character's own bank");
    c.that(MAIN.contains("voice::Bank::of(own, shown.profile.voice.is_some())"), "and applies this rule to it");
    c.that(MAIN.contains("banks.pick(shown.id.as_str(), n,"), "keeping it while she is the one shown");
    c.that(MAIN.contains(".manage(voice::Shown::default())"), "in a holder the app manages");
}

// ---------------------------------------------------------------------------------------------
// pet_settings: the settings file, what each window may ask for, and the settings applied the
// moment they change (Docs/DESIGN_Settings.md). The file and the rule are checked as behaviour; the
// pages are pinned as text, like the rest of `pet_page`, because this crate cannot run them.

/// The settings window, compiled in from `frontendDist` like the pet's own page.
const SETTINGS_PAGE: &str = include_str!("../../ui/settings.html");

/// A folder of its own under the temp folder, removed when dropped.
struct TempHome(PathBuf);

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_home(tag: &str) -> TempHome {
    let root = std::env::temp_dir().join(format!("vrpet-settings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    TempHome(root)
}

/// `{}` with one field set at a dotted path.
fn one_field(path: &str, value: Value) -> Value {
    let mut root = json!({});
    let mut at = &mut root;
    let keys: Vec<&str> = path.split('.').collect();
    for key in &keys[..keys.len() - 1] {
        at = &mut at[*key];
        if !at.is_object() {
            *at = json!({});
        }
    }
    at[keys[keys.len() - 1]] = value;
    root
}

fn file_parse(raw: &Value) -> (Settings, Vec<settings::Problem>) {
    Settings::parse(raw, Mode::File)
}

fn the_defaults_are_the_designs(c: &mut Checks) {
    // Docs/DESIGN_Settings.md 1, with no character chosen.
    let design = json!({
        "volume": 0.8,
        "scale": { "window": 1.0, "figure": 1.0, "ui": 1.0 },
        "switches": { "voice": true, "fillers": true, "mic": true, "follow": true, "on_top": true, "autostart": false },
        "speech": { "kind": "local" },
        "stt": { "pack": "sense-voice-zh-en-ja-ko-yue-int8-2024-07-17" },
        "chat_model": "haiku",
        "look": { "bubble_opacity": 0.92, "font_size": 13, "theme": "paper" },
        "settings_tab": "character",
    });
    c.eq(Settings::default().to_value(), design.clone(), "the defaults are the design's example");
    let (read, problems) = file_parse(&json!({}));
    c.eq(read, Settings::default(), "an empty file is the defaults");
    c.eq(problems, Vec::new(), "and nothing is wrong with it");

    let mut example = design;
    example["character"] = json!("char-20261107");
    let (read, problems) = file_parse(&example);
    c.eq(problems, Vec::new(), "the design's example reads with nothing wrong");
    c.eq(read.character.map(|id| id.to_string()), Some("char-20261107".to_string()), "and names her");

    for b in settings::NUMBERS {
        c.that(b.low <= b.default && b.default <= b.high, format!("{}'s default {} is in its range", b.field, b.default));
    }
}

fn each_range_holds_its_ends_and_refuses_past_them(c: &mut Checks) {
    // `scale.figure` beside `scale.window`, because alone it is an old file's window
    // (`an_old_files_figure_is_the_window`).
    let one_field = |field: &str, value: Value| {
        let mut raw = one_field(field, value);
        if field == settings::FIGURE.field {
            raw["scale"]["window"] = json!(settings::WINDOW.default);
        }
        raw
    };
    for b in settings::NUMBERS {
        let pointer = format!("/{}", b.field.replace('.', "/"));
        for end in [b.low, b.high] {
            let (read, problems) = file_parse(&one_field(b.field, json!(end)));
            c.eq(read.to_value().pointer(&pointer).and_then(Value::as_f64), Some(end), format!("{} takes {end}", b.field));
            c.eq(problems, Vec::new(), format!("{} = {end} is nothing wrong", b.field));
        }
        let step = if b.integer { 1.0 } else { 0.01 };
        for past in [b.low - step, b.high + step] {
            let (read, problems) = file_parse(&one_field(b.field, json!(past)));
            c.eq(read, Settings::default(), format!("{} = {past} falls back to the defaults", b.field));
            c.that(
                problems.len() == 1 && problems[0].field == b.field && problems[0].why.contains("不在"),
                format!("and says {} is out of range: {problems:?}", b.field),
            );
        }
    }
}

fn a_broken_field_falls_back_alone_and_says_why(c: &mut Checks) {
    let custom = |extra: Value| {
        let mut v = json!({ "kind": "custom", "url": "https://tts.example.com/v1", "model": "m", "voice": "v" });
        if let (Some(v), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
            v.extend(extra.clone());
        }
        v
    };
    let cases: Vec<(&str, Value, &str)> = vec![
        ("volume", json!("loud"), "volume"),
        ("look.font_size", json!(13.5), "look.font_size"),
        ("switches.mic", json!("yes"), "switches.mic"),
        ("scale", json!(3), "scale"),
        ("chat_model", json!("opus"), "chat_model"),
        ("stt.pack", json!("../x"), "stt.pack"),
        ("character", json!("Bad Name"), "character"),
        ("look.theme", json!("../night"), "look.theme"),
        ("settings_tab", json!(3), "settings_tab"),
        ("speech", custom(json!({ "url": "http://tts.example.com/v1" })), "speech.url"),
        ("speech", custom(json!({ "key_env": "sk-live-abc123" })), "speech.key_env"),
        ("speech", custom(json!({ "model": "" })), "speech.model"),
        ("speech", json!({ "kind": "cloud" }), "speech.kind"),
    ];
    for (path, value, field) in cases {
        // Beside a field that is fine, which must survive the broken one.
        let mut raw = one_field(path, value.clone());
        if path != "volume" {
            raw["volume"] = json!(0.3);
        }
        let (read, problems) = file_parse(&raw);
        let mut want = Settings::default();
        if path != "volume" {
            want.volume = 0.3;
        }
        c.eq(read, want, format!("{path} = {value} falls back and nothing else changes"));
        c.that(
            problems.iter().any(|p| p.field == field && !p.why.is_empty()),
            format!("and the reason is recorded under {field}: {problems:?}"),
        );
    }

    let (read, problems) = file_parse(&json!([1, 2]));
    c.eq(read, Settings::default(), "a file that is not an object is the defaults");
    c.eq(problems.len(), 1, "with one reason for the whole file");
}

fn a_custom_speech_service_is_checked(c: &mut Checks) {
    for url in ["https://tts.example.com/v1", "http://127.0.0.1:8880/v1", "http://[::1]:8880/v1", "https://a.b"] {
        c.that(settings::check_url(url).is_ok(), format!("{url} is a service she may speak through"));
    }
    for url in [
        "http://tts.example.com/v1",
        "http://127.0.0.1@evil.example/v1",
        "https://user:key@tts.example.com/v1",
        "ftp://127.0.0.1/v1",
        "tts.example.com/v1",
        "https:///v1",
        "https://a b/v1",
        "http://127.0.0.2/v1",
        // Refused by the core when it starts (ChatCore/src/voice.rs `speech_url`), so refused here
        // too, where the user still sees the address.
        "http://localhost:8880/v1",
        "HTTPS://A.B/v1",
        "https://tts.example.com/v1?key=x",
        "https://tts.example.com/v1#x",
    ] {
        c.that(settings::check_url(url).is_err(), format!("{url} is refused"));
    }
    for name in ["MY_TTS_KEY", "_K", "OPENAI_API_KEY", &"K".repeat(40)] {
        c.that(settings::check_env_name(name).is_ok(), format!("{name} is a variable's name"));
    }
    for name in ["sk-abc123", "1KEY", "MY KEY", "", "my_key", &"K".repeat(41)] {
        c.that(settings::check_env_name(name).is_err(), format!("{name:?} is not"));
    }
    let named = |model: &str| {
        let raw = json!({ "speech": { "kind": "custom", "url": "https://a.b/v1", "model": model, "voice": "v" } });
        file_parse(&raw).1.iter().any(|p| p.field == "speech.model")
    };
    for model in ["tts-1", "gpt-4o-mini-tts", "org/model:v2", "a.b_c"] {
        c.that(!named(model), format!("{model} is a model name the core takes"));
    }
    for model in ["tts 1", "模型", &"m".repeat(65)] {
        c.that(named(model), format!("{model:?} is one the core refuses, and so is the pet"));
    }

    let raw = json!({ "speech": { "kind": "custom", "url": "https://tts.example.com/v1", "model": "tts-1", "voice": "nova", "key_env": "MY_TTS_KEY" } });
    let (read, problems) = file_parse(&raw);
    c.eq(problems, Vec::new(), "a whole custom service reads with nothing wrong");
    c.eq(
        read.speech.clone(),
        settings::Speech::Custom {
            url: "https://tts.example.com/v1".into(),
            model: "tts-1".into(),
            voice: "nova".into(),
            key_env: Some("MY_TTS_KEY".into()),
        },
        "and is the service written",
    );
    c.eq(read.to_value()["speech"].clone(), raw["speech"].clone(), "and is written back as it was read");
    let (read, problems) = file_parse(&json!({ "speech": { "kind": "custom", "url": "http://127.0.0.1:8880/v1", "model": "m", "voice": "v", "key_env": "" } }));
    c.that(problems.is_empty() && matches!(read.speech, settings::Speech::Custom { key_env: None, .. }), "an empty key name is no key");
}

fn the_file_round_trips_and_keeps_what_it_does_not_know(c: &mut Checks) {
    let home = temp_home("trip");
    let file = home.0.join(settings::FILE);
    let _ = std::fs::write(
        &file,
        r#"{"character":"char-20261107","volume":0.5,"theme":"dark","scale":{"window":1.2,"figure":0.8,"later":true},"speech":{"kind":"local"}}"#,
    );
    let store = settings::Store::open(Some(home.0.clone()));
    let first = store.loaded();
    c.eq(first.problems.clone(), Vec::new(), "a file from another version reads with nothing wrong");
    c.eq(
        (first.settings.volume, first.settings.scale.window, first.settings.scale.figure, first.settings.scale.ui),
        (0.5, 1.2, 0.8, 1.0),
        "with its values and the defaults for what it lacks",
    );

    let mut request = first.settings.to_value();
    request["volume"] = json!(0.3);
    request["chat_model"] = json!("sonnet");
    request["switches"]["follow"] = json!(false);
    let set = store.set(&request);
    c.that(set.is_ok(), format!("a whole request is saved: {set:?}"));

    let raw = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
    c.eq(raw.get("theme"), Some(&json!("dark")), "a key this version does not know is kept");
    c.eq(raw.pointer("/scale/later"), Some(&json!(true)), "at any depth");
    c.eq(raw.get("volume"), Some(&json!(0.3)), "and the change is written");
    c.eq(raw.get("chat_model"), Some(&json!("sonnet")), "every change");
    c.eq(raw.pointer("/switches/follow"), Some(&json!(false)), "nested ones too");
    c.eq(raw.get("character"), Some(&json!("char-20261107")), "and her choice is still there");
    c.that(!home.0.join(format!("{}.new", settings::FILE)).exists(), "nothing is left beside the file");

    let again = settings::Store::open(Some(home.0.clone())).loaded();
    c.eq(Some(again.clone()), set.ok(), "what a restart reads is what was saved");
    c.eq(again.problems, Vec::new(), "with nothing wrong");

    let _ = std::fs::write(&file, b"{ not json");
    let broken = settings::Store::open(Some(home.0.clone())).loaded();
    c.eq(broken.settings, Settings::default(), "a file that is not JSON is the defaults");
    c.that(broken.problems.len() == 1 && broken.problems[0].why.contains("JSON"), format!("and says so: {:?}", broken.problems));
}

/// Before 2026-11-08 `scale.figure` sized the whole window. A file from then has that key and no
/// `scale.window`, and the user left the pet at that size, so it is read as the window, with her
/// own size at its default, and the settings window is told. Saving once writes the new keys.
fn an_old_files_figure_is_the_window(c: &mut Checks) {
    let (read, problems) = file_parse(&json!({ "scale": { "figure": 1.5, "ui": 1.2 } }));
    c.eq((read.scale.window, read.scale.figure, read.scale.ui), (1.5, settings::FIGURE.default, 1.2), "an old file's figure is the window");
    c.that(
        problems.len() == 1 && problems[0].field == "scale.figure" && problems[0].why.contains("scale.window = 1.5"),
        format!("and the settings window is told how it was read: {problems:?}"),
    );
    let (read, problems) = file_parse(&json!({ "scale": { "figure": 1.9 } }));
    c.eq((read.scale.window, read.scale.figure), (settings::WINDOW.default, settings::FIGURE.default), "an old value past the window's range falls back");
    c.that(problems.iter().any(|p| p.why.contains("不在 0.6 到 1.6")), format!("by the window's range, not the figure's: {problems:?}"));
    let (read, problems) = file_parse(&json!({ "scale": { "window": 1.0, "figure": 1.5 } }));
    c.eq((read.scale.window, read.scale.figure), (1.0, settings::FIGURE.default), "a new file's figure is her own size, with its own range");
    c.that(problems.len() == 1 && problems[0].why.contains("不在 0.75 到 1.3"), format!("so 1.5 is past it: {problems:?}"));

    let home = temp_home("old-scale");
    let file = home.0.join(settings::FILE);
    let _ = std::fs::write(&file, r#"{"scale":{"figure":1.5}}"#);
    let store = settings::Store::open(Some(home.0.clone()));
    let saved = store.set(&store.current().to_value());
    c.that(saved.as_ref().is_ok_and(|l| l.problems.is_empty()), format!("saving once clears the note: {saved:?}"));
    let raw = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
    c.eq(raw.get("scale").cloned(), Some(json!({ "window": 1.5, "figure": 1.0, "ui": 1.0 })), "and writes the sizes the new way");
    let again = settings::Store::open(Some(home.0.clone())).loaded();
    c.eq((again.settings.scale.window, again.problems.len()), (1.5, 0), "which a restart reads as they were, with nothing to say");

    let mut request = Settings::default().to_value();
    if let Some(o) = request["scale"].as_object_mut() {
        o.remove("window");
    }
    match store.set(&request) {
        Ok(_) => c.that(false, "a request without scale.window is refused, not read the old way"),
        Err(why) => c.that(why.contains("scale.window"), format!("a request without scale.window is refused, naming it: {why}")),
    }
}

fn a_request_is_whole_or_refused(c: &mut Checks) {
    let home = temp_home("request");
    let file = home.0.join(settings::FILE);
    let _ = std::fs::write(&file, r#"{"character":"char-20261107","volume":0.5}"#);
    let store = settings::Store::open(Some(home.0.clone()));
    let before = std::fs::read(&file).unwrap_or_default();
    let whole = store.current().to_value();

    let mut missing = whole.clone();
    if let Some(o) = missing.as_object_mut() {
        o.remove("look");
    }
    let mut no_switch = whole.clone();
    if let Some(o) = no_switch["switches"].as_object_mut() {
        o.remove("mic");
    }
    for (what, request, field) in [
        ("a request without look", missing, "look"),
        ("a request without switches.mic", no_switch, "switches.mic"),
        ("a request with the volume at 2", one_field_over(&whole, "volume", json!(2)), "volume"),
        ("a request with a remote http service", one_field_over(&whole, "speech", json!({ "kind": "custom", "url": "http://x.example/v1", "model": "m", "voice": "v" })), "speech.url"),
    ] {
        match store.set(&request) {
            Ok(_) => c.that(false, format!("{what} is refused")),
            Err(why) => c.that(why.contains(field), format!("{what} is refused, naming {field}: {why}")),
        }
        c.eq(std::fs::read(&file).unwrap_or_default(), before.clone(), format!("and after {what} the file is untouched"));
        c.eq(store.current().volume, 0.5, format!("and after {what} nothing changed"));
    }

    let elsewhere = one_field_over(&whole, "character", json!("someone-else"));
    let set = store.set(&elsewhere);
    c.that(set.is_ok(), "a request that names another character is otherwise saved");
    c.eq(
        store.current().character.map(|id| id.to_string()),
        Some("char-20261107".to_string()),
        "and she is still the one chosen, since only choosing her changes her",
    );
    c.that(settings::Store::open(None).set(&whole).is_err(), "with no home there is nowhere to save, and saying so is the answer");
}

fn one_field_over(base: &Value, key: &str, value: Value) -> Value {
    let mut v = base.clone();
    v[key] = value;
    v
}

/// Each window's list, written out here as the design gives it (Docs/DESIGN_Settings.md 2), and
/// asked of the rule one command at a time, so a command added to the wrong list or registered and
/// listed nowhere turns this red.
fn each_window_gets_exactly_its_commands(c: &mut Checks) {
    let pet = [
        "fleet", "set_regions", "report", "save_shot", "core_state", "say", "filler", "stt_state", "mic_press", "mic_release", "quit",
        "characters", "greet", "get_settings", "open_settings", "themes", cast::SCHEME, crate::themes::SCHEME,
    ];
    let settings_window = [
        "get_settings",
        "set_settings",
        "characters",
        "choose_character",
        "open_characters_folder",
        "character_profile",
        "save_profile",
        "copy_character",
        "stt_packs",
        "themes",
        "open_themes_folder",
        crate::themes::SCHEME,
    ];
    let registered = registered_commands();
    c.that(registered.len() >= 24, format!("the handler registers the commands: {registered:?}"));

    let mut asked: Vec<&str> = registered.iter().map(String::as_str).collect();
    asked.extend([cast::SCHEME, crate::themes::SCHEME, "no_such_command", "plugin:event|listen"]);
    for command in &asked {
        c.eq(crate::refuse(crate::OVERLAY, command).is_none(), pet.contains(command), format!("the pet's window may ask for {command}"));
        c.eq(
            crate::refuse(crate::SETTINGS_WINDOW, command).is_none(),
            settings_window.contains(command),
            format!("the settings window may ask for {command}"),
        );
    }
    for command in &registered {
        c.that(
            pet.contains(&command.as_str()) || settings_window.contains(&command.as_str()),
            format!("{command} is registered and one of the windows may ask for it"),
        );
    }
    for (window, list) in crate::ALLOWED {
        for command in *list {
            c.that(
                registered.iter().any(|r| r == command) || [cast::SCHEME, crate::themes::SCHEME].contains(command),
                format!("{window} may ask for {command}, which is a registered command or scheme"),
            );
        }
    }
    c.that(!crate::ALLOWED[0].1.iter().any(|n| ["character_profile", "save_profile", "copy_character", "stt_packs"].contains(n)), "the pet's own window is given none of the editor's commands");
    for refused in ["say", "greet", "mic_press", "filler", "quit", cast::SCHEME] {
        match crate::refuse(crate::SETTINGS_WINDOW, refused) {
            Some(why) => c.that(why.contains(refused), format!("the settings window is refused {refused}, by name: {why}")),
            None => c.that(false, format!("the settings window is refused {refused}")),
        }
    }
    for label in ["log", "", "Settings", "settings ", "main2"] {
        c.that(crate::refuse(label, "get_settings").is_some(), format!("a window called {label:?} may ask for nothing"));
    }
    c.eq(crate::SETTINGS_WINDOW, "settings", "the settings window's label");

    // The settings window needs no core permission: its commands are the app's own, which the rule
    // above governs. A capability naming it would hand it Tauri's window and event APIs as well.
    let windows = serde_json::from_str::<Value>(CAPABILITIES)
        .ok()
        .and_then(|v| v.get("windows").cloned())
        .unwrap_or(Value::Null);
    c.eq(windows, json!(["main"]), "the one capability is the pet's window's");
}

/// Every command the handler registers, by name.
fn registered_commands() -> Vec<String> {
    MAIN.find("generate_handler![")
        .map(|at| MAIN[at + "generate_handler![".len()..].chars().take_while(|&ch| ch != ']').collect::<String>())
        .unwrap_or_default()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect()
}

/// The commands `source` invokes, by the name in `invoke("<name>"`.
fn invoked(source: &str) -> Vec<String> {
    source
        .match_indices("invoke(\"")
        .filter_map(|(at, _)| source[at + "invoke(\"".len()..].split_once('"').map(|(name, _)| name.to_string()))
        .collect()
}

/// A page that calls a command its window is refused fails at the user's click and nowhere earlier,
/// as an error the page only logs.
fn each_page_asks_only_for_what_its_window_may(c: &mut Checks) {
    for (page, label, source) in [("index.html", crate::OVERLAY, PAGE), ("settings.html", crate::SETTINGS_WINDOW, SETTINGS_PAGE)] {
        let names = invoked(source);
        c.that(names.len() >= 3, format!("{page} invokes commands: {names:?}"));
        for name in names {
            c.that(crate::refuse(label, &name).is_none(), format!("{page} asks for {name}, which its window may"));
        }
    }
    c.that(!invoked(SETTINGS_PAGE).iter().any(|n| n == "say" || n == "mic_press"), "the settings window never asks to talk or listen");
}

/// The attribute `name` of the tag on the settings page that carries `data-field="<field>"`.
fn field_attr(field: &str, name: &str) -> Option<String> {
    let at = SETTINGS_PAGE.find(&format!("data-field=\"{field}\""))?;
    let start = SETTINGS_PAGE[..at].rfind('<')?;
    let end = at + SETTINGS_PAGE[at..].find('>')?;
    let tag = &SETTINGS_PAGE[start..end];
    let value = tag.split_once(&format!(" {name}=\""))?.1.split_once('"')?.0;
    Some(value.to_string())
}

/// The sliders are Rust's ranges, so a slider can never send a value Rust refuses, and every field
/// the design names has its control (Docs/DESIGN_Settings.md 1 and lin's six sections).
fn the_settings_page_and_rust_agree_on_every_field(c: &mut Checks) {
    c.that(SETTINGS_PAGE.len() > 3000, format!("settings.html has content: {} bytes", SETTINGS_PAGE.len()));
    for b in settings::NUMBERS {
        let range = (field_attr(b.field, "min"), field_attr(b.field, "max"));
        let parsed = (
            range.0.as_deref().and_then(|v| v.parse::<f64>().ok()),
            range.1.as_deref().and_then(|v| v.parse::<f64>().ok()),
        );
        c.eq(parsed, (Some(b.low), Some(b.high)), format!("the {} slider runs over Rust's range", b.field));
        if b.integer {
            c.eq(field_attr(b.field, "step").as_deref(), Some("1"), format!("and {} moves in whole steps", b.field));
        }
    }
    for (name, _) in settings::SWITCHES {
        c.that(field_attr(&format!("switches.{name}"), "type").as_deref() == Some("checkbox"), format!("switches.{name} is a checkbox"));
    }
    c.that(
        SETTINGS_PAGE.contains("data-field=\"switches.autostart\" />") && !SETTINGS_PAGE.contains("之後才會做好"),
        "autostart is a switch that works, and no placeholder is left",
    );
    c.that(field_attr("stt.pack", "id").as_deref() == Some("sttPack"), "the speech recognition pack has a control");
    for needle in ["name=\"speech\" value=\"local\"", "name=\"speech\" value=\"custom\"", "name=\"chat_model\" value=\"haiku\"", "name=\"chat_model\" value=\"sonnet\""] {
        c.that(SETTINGS_PAGE.contains(needle), format!("the page offers {needle}"));
    }
    for id in ["speechUrl", "speechModel", "speechVoice", "speechKey"] {
        c.that(SETTINGS_PAGE.contains(&format!("id=\"{id}\"")), format!("the custom service has {id}"));
    }
    // The tabs, in Rust's order and with lin's names, each with the pane it shows (使用者 2026-11-08).
    let tabs: Vec<(String, String)> = SETTINGS_PAGE
        .match_indices("role=\"tab\" id=\"tab-")
        .filter_map(|(at, _)| {
            let tag = &SETTINGS_PAGE[at..at + SETTINGS_PAGE[at..].find("</button>")?];
            let name = tag.split_once("data-tab=\"")?.1.split_once('"')?.0;
            Some((name.to_string(), tag.rsplit_once('>')?.1.to_string()))
        })
        .collect();
    let want: Vec<(String, String)> = settings::Tab::ALL
        .iter()
        .zip(["角色", "聲音", "外觀", "開關", "模型"])
        .map(|(t, label)| (t.name().to_string(), label.to_string()))
        .collect();
    c.eq(tabs, want, "the tab bar is Rust's tabs in order, each with its name");
    for tab in settings::Tab::ALL {
        let pane = format!("id=\"pane-{0}\" role=\"tabpanel\" data-pane=\"{0}\" aria-labelledby=\"tab-{0}\"", tab.name());
        c.that(SETTINGS_PAGE.contains(&pane), format!("the {} tab has its pane", tab.name()));
    }
    // Which pane each control is in: the last pane opened before it, since panes do not nest.
    let pane_of = |needle: &str| {
        let at = SETTINGS_PAGE.find(needle)?;
        let open = SETTINGS_PAGE[..at].rfind("data-pane=\"")?;
        Some(SETTINGS_PAGE[open + "data-pane=\"".len()..].split_once('"')?.0.to_string())
    };
    for (needle, tab) in [
        ("id=\"cast\"", "character"),
        ("id=\"profileEditor\"", "character"),
        ("data-field=\"volume\"", "voice"),
        ("<h2>介面外觀</h2>", "look"),
        ("data-field=\"look.bubble_opacity\"", "look"),
        ("data-field=\"look.font_size\"", "look"),
        ("<h2>大小</h2>", "look"),
        ("data-field=\"scale.figure\"", "look"),
        ("data-field=\"scale.window\"", "look"),
        ("data-field=\"scale.ui\"", "look"),
        ("data-field=\"switches.autostart\"", "switches"),
        ("id=\"sttPack\"", "models"),
        ("name=\"chat_model\"", "models"),
    ] {
        c.eq(pane_of(needle).as_deref(), Some(tab), format!("{needle} is on the {tab} tab"));
    }
    c.that(
        css_rule(SETTINGS_PAGE, ".tabs").is_some_and(|b| b.contains("grid-template-columns: repeat(5, 1fr);")),
        "the five tabs share one row in equal cells, so the bar fits 375 px",
    );
    c.that(SETTINGS_PAGE.contains(">打開角色資料夾</button>"), "and the button that opens the characters folder");
    for id in ["profileName", "profilePersona", "profileGreeting", "profileFillers", "profileSave", "copyId", "copyButton"] {
        c.that(SETTINGS_PAGE.contains(&format!("id=\"{id}\"")), format!("the profile editor has {id}"));
    }
    c.that(!SETTINGS_PAGE.contains("PLACEHOLDER"), "and no placeholder is left");
    c.that(MAIN.contains("settings: serde_json::Value"), "the form goes to Rust under the argument name Rust reads");
}

/// The settings window opens on the tab it was last on, which Rust keeps with the other settings.
fn the_window_opens_on_the_last_tab(c: &mut Checks) {
    let (read, problems) = file_parse(&json!({ "settings_tab": "look" }));
    c.eq((read.settings_tab, problems.len()), (settings::Tab::Look, 0), "a saved tab is read");
    let (read, problems) = file_parse(&json!({ "settings_tab": "fleet" }));
    c.eq(read.settings_tab, settings::Tab::Character, "one that is not a tab is the first");
    c.that(problems.iter().any(|p| p.field == "settings_tab"), format!("and says why: {problems:?}"));

    let home = temp_home("tab");
    let store = settings::Store::open(Some(home.0.clone()));
    let mut request = store.current().to_value();
    request["settings_tab"] = json!("models");
    c.that(store.set(&request).is_ok(), "moving to a tab is saved like any change");
    c.eq(settings::Store::open(Some(home.0.clone())).current().settings_tab, settings::Tab::Models, "and the next start opens there");

    let load = SETTINGS_PAGE.find("invoke(\"get_settings\")").map(|at| &SETTINGS_PAGE[at..]).unwrap_or("");
    c.that(load.contains("showTab(current.settings_tab);"), "the page shows the saved tab when Rust answers");
    let choose = source_block(SETTINGS_PAGE, "function chooseTab(");
    c.that(choose.contains("change((s) => (s.settings_tab = name), true);"), "and saves each move, quietly");
    let change = source_block(SETTINGS_PAGE, "function change(");
    c.that(change.contains("if (!quiet) worthSaying = true;"), "quiet meaning no \"已存。\" for a tab");
}

/// Quick changes all land. A request built from Rust's last answer while another was still on its
/// way lacked that one's change and undid it when it landed: driven live, eight quick changes kept
/// four. So every request is a copy of the user's own running state, and Rust's answer is never
/// the base of the next one.
fn the_form_sends_every_change_in_order(c: &mut Checks) {
    let change = source_block(SETTINGS_PAGE, "function change(");
    c.that(change.contains("edit(wanted);"), "a change is applied to what the user has asked for so far");
    c.that(!change.contains("current"), "and not to Rust's last answer");
    let flush = source_block(SETTINGS_PAGE, "async function flush(");
    c.that(
        flush.contains("invoke(\"set_settings\", { settings: JSON.parse(JSON.stringify(wanted)) })"),
        "each request is a copy of that, whole",
    );
    c.that(flush.contains("while (dirty) {"), "and one more is sent while anything changed since the last went out");
    c.that(flush.contains("if (!dirty) wanted = JSON.parse(JSON.stringify(current));"), "a refused change is dropped only when nothing newer waits");
}

/// What the pet's page does with each field it is sent. Pinned as text; the live check is what
/// runs it.
fn the_pet_applies_each_setting_it_is_sent(c: &mut Checks) {
    let name = MAIN
        .lines()
        .find_map(|l| l.strip_prefix("const SETTINGS_EVENT: &str = \"")?.strip_suffix("\";"))
        .unwrap_or("");
    c.eq(name, "settings", "main.rs names the settings event");
    let listen = PAGE.find(&format!("listen(\"{name}\", "));
    let ask = PAGE.find("invoke(\"get_settings\")");
    let mount = PAGE.find("Character.mount(");
    c.that(
        matches!((mount, listen, ask), (Some(m), Some(l), Some(a)) if m < l && l < a),
        format!("after the character mounts, the page listens for changes and then asks once: {mount:?} {listen:?} {ask:?}"),
    );
    if let Some(at) = listen {
        let before = &PAGE[..at];
        c.that(before.rfind("try {").is_some_and(|t| t > before.rfind("\n}").unwrap_or(0)), "and that wiring sits inside a try");
    }

    let apply = page_block("function applySettings(");
    for line in [
        "Speaker.configure({ volume: s.volume, voice: s.switches.voice, fillers: s.switches.fillers });",
        "micEl.hidden = !s.switches.mic;",
        "root.setProperty(\"--figure\", String(s.scale.figure));",
        "root.setProperty(\"--ui\", String(s.scale.ui));",
        "root.setProperty(\"--say-size\", s.look.font_size + \"px\");",
        "root.setProperty(\"--bubble-opacity\", String(s.look.bubble_opacity));",
        "reportRegions();",
    ] {
        c.that(apply.contains(line), format!("applySettings does {line}"));
    }
    c.that(
        PAGE.contains("Character.look(() => (settings.switches.follow ? cursorAt : null));"),
        "the look source gives nothing while following is off",
    );

    // Every name the page reads is one Rust writes.
    let sent = Settings::default().to_value();
    for pointer in ["/volume", "/scale/ui", "/scale/figure", "/look/font_size", "/look/bubble_opacity", "/switches/voice", "/switches/fillers", "/switches/mic", "/switches/follow"] {
        c.that(sent.pointer(pointer).is_some(), format!("Rust sends {pointer}"));
    }

    // The stylesheet reads what applySettings writes.
    for (selector, needle) in [
        (".chat-say", "font-size: calc(var(--say-size) * var(--ui));"),
        (":root", "--paper-seen: color-mix(in srgb, var(--paper) calc(var(--bubble-opacity) * 100%), transparent);"),
        (".chat-box", "background-color: var(--paper-seen);"),
        (".chat-log", "background-color: var(--paper-seen);"),
        (".chat-choice", "background-color: var(--paper-seen);"),
        (".chat-input", "background-color: color-mix(in srgb, var(--controls) calc(var(--bubble-opacity) * 100%), transparent);"),
        (".chat-field", "font-size: calc(13px * var(--ui));"),
        ("#controls button", "font-size: calc(11px * var(--ui));"),
        (".chat-choice", "font-size: calc(12px * var(--ui));"),
        (".chat-mic", "font-size: calc(12px * var(--ui));"),
    ] {
        c.that(css_rule(PAGE, selector).is_some_and(|body| body.contains(needle)), format!("{selector} reads {needle}"));
    }

    // One gain for her voice and the fillers, and the two switches.
    let start = speaker_fn("async function start(");
    c.that(start.contains("src.connect(output());") && !start.contains("destination"), "every clip goes through the one gain");
    c.eq(PAGE.matches("createGain(").count(), 1, "and there is one");
    let configure = speaker_fn("function configure(");
    c.that(configure.contains("if (gain) gain.gain.value = volume;"), "the volume sets the gain");
    c.that(configure.contains("if (voiced && !s.voice) stop();"), "switching her voice off stops what she is saying");
    let speak = speaker_fn("function speak(");
    let muted = speak.find("if (!voiced) return Promise.resolve(false);");
    let started = speak.find("return start(");
    c.that(matches!((muted, started), (Some(m), Some(s)) if m < s), "a muted answer is not played");
    c.that(speaker_fn("function queue(").contains("if (!voiced) return Promise.resolve(false);"), "and neither are its later parts");
    let filler = speaker_fn("async function filler(");
    let off = filler.find("if (!fillers) {");
    let fetched = filler.find("invoke(\"filler\")");
    c.that(matches!((off, fetched), (Some(o), Some(f)) if o < f), "with fillers off none is fetched or played");
}

fn rust_applies_its_share_and_sends_the_rest(c: &mut Checks) {
    let press = source_block(MAIN, "async fn mic_press(");
    let gate = press.find("if !store.current().switches.mic {");
    let take = press.find("held.take(");
    c.that(matches!((gate, take), (Some(g), Some(t)) if g < t), "mic_press is refused while the microphone is off, before it takes the lease");

    let set = source_block(MAIN, "fn set_settings(");
    let saved = set.find("store.set_with(&settings,");
    let applied = set.find("apply(&app, &before, &after.settings);");
    c.that(matches!((saved, applied), (Some(s), Some(a)) if s < a), "a request is saved before anything is applied, and only if it was");

    let apply = source_block(MAIN, "fn apply(");
    for (needle, what) in [
        ("win.set_always_on_top(after.switches.on_top)", "on_top sets the window's place"),
        ("fit_window(&win, after.scale.window);", "scale.window sizes the window"),
        ("app.state::<press::Held>().release()", "the microphone switched off ends a press"),
        ("app.emit_to(OVERLAY, SETTINGS_EVENT, after)", "and the pet's window, and only it, is sent the rest"),
    ] {
        c.that(apply.contains(needle), what);
    }
    let fit = source_block(MAIN, "fn fit_window(");
    for needle in ["settings::window_rect(", "win.set_size(", "win.set_position(", "win.set_zoom(factor)"] {
        c.that(fit.contains(needle), format!("fit_window calls {needle}"));
    }
    c.that(MAIN.contains("fit_window(&win, store.window());"), "at start, and every load of the pet's page runs it again");
    c.that(MAIN.contains("app.try_state::<settings::Store>()"), "a load that starts before the settings exist is left to setup");
    c.that(
        MAIN.contains("hit::css((cursor.x, cursor.y), (origin.x as f64, origin.y as f64), scale * zoom)"),
        "the cursor reaches the page in its zoomed CSS pixels",
    );
    c.that(MAIN.contains("win.set_always_on_top(store.current().switches.on_top)"), "the saved place on top is applied at start");

    let conf: Value = serde_json::from_str(CONF).unwrap_or(Value::Null);
    let base = (conf.pointer("/app/windows/0/width").and_then(Value::as_f64), conf.pointer("/app/windows/0/height").and_then(Value::as_f64));
    c.eq(base, (Some(settings::WINDOW_BASE.0), Some(settings::WINDOW_BASE.1)), "the window's base size is its configured size");
}

fn the_window_grows_from_its_bottom_left_corner(c: &mut Checks) {
    // 460x620 at a monitor scale of 1.25 is 575x775 physical, which is what was measured
    // (index.html, `regionOf`).
    let at = (100, 500);
    let size = (575, 775);
    let bottom = at.1 + size.1 as i32;
    c.eq(settings::window_rect(at, size, 1.0, 1.25, None), (at, size), "at 1.0 nothing moves");
    let (to, big) = settings::window_rect(at, size, 1.6, 1.25, None);
    c.eq(big, (920, 1240), "at 1.6 the window is 1.6 times the base");
    c.eq((to.0, to.1 + big.1 as i32), (at.0, bottom), "and its left edge and bottom stay where they were");
    let (to, small) = settings::window_rect(at, size, 0.6, 1.25, None);
    c.eq(small, (345, 465), "at 0.6 it is 0.6 times");
    c.eq((to.0, to.1 + small.1 as i32), (at.0, bottom), "with the same corner");
    let (back, again) = settings::window_rect(to, small, 1.0, 1.25, None);
    c.eq((back, again), (at, size), "and going back to 1.0 puts it back exactly");
    let (to, big) = settings::window_rect(at, size, 1.6, 2.0, None);
    c.eq(big, (1472, 1984), "the monitor's scale multiplies in");
    c.eq(to.1 + big.1 as i32, bottom, "and the bottom still stays");
}

fn the_characters_folder_is_made_when_opened(c: &mut Checks) {
    let home = temp_home("folder");
    let made = settings::characters_dir(&home.0);
    c.eq(made.clone().ok(), Some(home.0.join(cast::USER_DIR)), "the folder is <home>/characters");
    c.that(home.0.join(cast::USER_DIR).is_dir(), "and it exists once asked for");
    c.eq(settings::characters_dir(&home.0).ok(), made.ok(), "and asking again finds it there");
    let open = source_block(MAIN, "fn open_characters_folder(");
    c.that(open.contains("settings::characters_dir(&home::dir()?)?") && open.contains("Command::new(\"explorer\").arg(&dir)"), "the button opens that folder in Explorer");
}

/// Item 1: which child a change restarts, and the environment it restarts with.
fn a_change_of_model_restarts_the_child_that_reads_it(c: &mut Checks) {
    let before = Settings::default();
    let mut sonnet = before.clone();
    sonnet.chat_model = settings::ChatModel::Sonnet;
    let custom = settings::Speech::Custom {
        url: "https://tts.example.com/v1".into(),
        model: "tts-1".into(),
        voice: "nova".into(),
        key_env: Some("MY_TTS_KEY".into()),
    };
    let mut remote = before.clone();
    remote.speech = custom.clone();
    let mut pack = before.clone();
    pack.stt.pack = "another-pack".into();
    let mut louder = before.clone();
    louder.volume = 0.3;
    louder.switches.mic = false;
    louder.scale.window = 1.4;
    louder.scale.figure = 1.2;

    let r = settings::restarts(&before, &sonnet);
    c.that(r.core.as_deref().is_some_and(|w| w.contains("sonnet")) && r.stt.is_none(), format!("the chat model restarts the core and says to what: {r:?}"));
    let r = settings::restarts(&before, &remote);
    c.that(r.core.as_deref().is_some_and(|w| w.contains("https://tts.example.com/v1")) && r.stt.is_none(), format!("a custom voice restarts the core: {r:?}"));
    let r = settings::restarts(&remote, &before);
    c.that(r.core.as_deref().is_some_and(|w| w.contains("本機")), format!("and so does going back to the local one: {r:?}"));
    let r = settings::restarts(&before, &pack);
    c.that(r.core.is_none() && r.stt.as_deref().is_some_and(|w| w.contains("another-pack")), format!("a pack restarts the speech input service alone: {r:?}"));
    c.eq(settings::restarts(&before, &louder), settings::Restarts::default(), "what applies at once restarts nothing");
    c.eq(settings::restarts(&before, &before), settings::Restarts::default(), "and the same settings restart nothing");

    let env = |s: &Settings| crate::core::env(s).into_iter().collect::<std::collections::BTreeMap<_, _>>();
    let local = env(&before);
    c.eq(local.get("VRCHATCORE_MODEL").cloned().flatten().as_deref(), Some("haiku"), "the model is always set");
    for name in ["VRCHATCORE_SPEECH_URL", "VRCHATCORE_SPEECH_MODEL", "VRCHATCORE_SPEECH_VOICE", "VRCHATCORE_SPEECH_KEY_ENV"] {
        c.eq(local.get(name).cloned(), Some(None), format!("a local voice removes {name}, whatever the pet inherited"));
    }
    let far = env(&remote);
    c.eq(far.get("VRCHATCORE_SPEECH_URL").cloned().flatten().as_deref(), Some("https://tts.example.com/v1"), "a custom voice sets its address");
    c.eq(far.get("VRCHATCORE_SPEECH_MODEL").cloned().flatten().as_deref(), Some("tts-1"), "its model");
    c.eq(far.get("VRCHATCORE_SPEECH_VOICE").cloned().flatten().as_deref(), Some("nova"), "its voice");
    c.eq(far.get("VRCHATCORE_SPEECH_KEY_ENV").cloned().flatten().as_deref(), Some("MY_TTS_KEY"), "and the NAME of its key's variable");
    c.eq(env(&sonnet).get("VRCHATCORE_MODEL").cloned().flatten().as_deref(), Some("sonnet"), "sonnet is passed as sonnet");

    // The key itself is never the pet's to move: set in the pet's environment, it is in nothing the
    // pet writes, sends or hands the core, and the core reads it on its own.
    let secret = "sk-live-0123456789abcdef";
    let home = temp_home("key");
    let store = settings::Store::open(Some(home.0.clone()));
    let mut request = store.current().to_value();
    request["speech"] = json!({ "kind": "custom", "url": "https://tts.example.com/v1", "model": "tts-1", "voice": "nova", "key_env": "MY_TTS_KEY" });
    let saved = store.set(&request);
    c.that(saved.is_ok(), format!("a custom service with a key's name is saved: {saved:?}"));
    let file = std::fs::read_to_string(home.0.join(settings::FILE)).unwrap_or_default();
    c.that(file.contains("MY_TTS_KEY"), "the file holds the variable's name");
    let mut sent = format!("{file}{}", serde_json::to_string(&store.loaded()).unwrap_or_default());
    for (name, value) in crate::core::env(&store.current()) {
        sent.push_str(&format!("{name}={}", value.unwrap_or_default()));
    }
    c.that(!sent.contains(secret) && !sent.contains("sk-"), "and no key is in the file, the event, or the core's environment");
    request["speech"]["key_env"] = json!(secret);
    c.that(store.set(&request).is_err(), "a key pasted where the name goes is refused, not saved");
    c.that(!std::fs::read_to_string(home.0.join(settings::FILE)).unwrap_or_default().contains(secret), "and never reaches the file");

    let launch = source_block(CORE, "fn launch(");
    for (needle, what) in [
        ("app.state::<settings::Store>().current()", "the core reads the settings each time it starts"),
        ("for (name, value) in env(&settings)", "and sets its environment from them"),
        ("cmd.env_remove(name);", "removing what the settings do not set"),
    ] {
        c.that(launch.contains(needle), what);
    }
    let restart = source_block(CORE, "pub fn restart(");
    let said = restart.find("set_state(app, CoreState::Restarting");
    let stopped = restart.find("core.piped.stop(child::STOP_WAIT)");
    let started = restart.find("start(app);");
    c.that(
        matches!((said, stopped, started), (Some(a), Some(b), Some(s)) if a < b && b < s),
        "a restart says why, stops the old core, then starts a new one",
    );
    let stt_launch = source_block(STT, "fn launch(");
    c.that(stt_launch.contains("cmd.env(PACK_ENV, &pack);"), "the speech input service is started with the pack chosen");
    c.eq(stt::PACK_ENV, "VRSTT_PACK", "under the name vrstt reads");
    let apply = source_block(MAIN, "fn apply(");
    c.that(
        apply.contains("std::thread::spawn(move || core::restart(&app, &why));") && apply.contains("std::thread::spawn(move || stt::restart(&app, &why));"),
        "a change restarts each child on a thread of its own",
    );
    c.that(source_block(MAIN, "async fn set_settings(").contains("apply(&app, &before, &after.settings);"), "from the settings window's request");

    // The page says why she is quiet, and listens again after.
    let on_core = page_block("function onCoreState(");
    c.that(on_core.contains("s.state === \"restarting\"") && on_core.contains("trouble("), "the pet's page says why the core is restarting");
    c.that(on_core.contains("coreDown = false"), "and a core that is back can be said down again later");
    let on_stt = page_block("function onSttState(");
    c.that(on_stt.contains("s.state === \"restarting\"") && on_stt.contains("setMic(MicState.Unavailable, String(s.why))"), "and the microphone why it is unavailable");
    c.eq(
        serde_json::to_value(CoreState::Restarting { why: "w".into() }).ok(),
        Some(json!({ "state": "restarting", "why": "w" })),
        "restarting reaches the page under the name it reads",
    );
    c.eq(
        CoreState::Restarting { why: "w".into() }.after(&json!({ "ev": "ready", "version": "1" })),
        Some(CoreState::Ready { version: "1".into() }),
        "and the new core's ready ends it",
    );
    c.eq(serde_json::to_value(SttState::Restarting { why: "w".into() }).ok(), Some(json!({ "state": "restarting", "why": "w" })), "the service's too");
}

/// Item 1: the restart's stop, against real processes. One that reads its stdin to the end exits
/// when it closes; one that never reads it is killed after the wait; and the end of a run the stop
/// retired is not reported as the child being gone.
fn a_restart_closes_stdin_waits_and_kills_what_stays(c: &mut Checks) {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let run = |piped: &Arc<crate::child::Piped>, program: &str, args: &[&str], gone: Arc<AtomicBool>| -> Option<std::thread::JoinHandle<Option<i32>>> {
        let mut child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let (stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
        let reaper = piped.adopt(stdin, child);
        let piped = Arc::clone(piped);
        Some(std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::BufReader::new(stdout), &mut std::io::sink());
            let code = reaper.wait();
            piped.end(&reaper, || gone.store(true, Ordering::SeqCst));
            code
        }))
    };

    let piped = Arc::new(crate::child::Piped::default());
    c.eq(piped.stop(Duration::from_millis(10)), crate::child::Stopped::Idle, "with nothing running a stop does nothing");

    // `findstr` reads its stdin to the end, like the core and the service.
    let gone = Arc::new(AtomicBool::new(false));
    let reader = run(&piped, "findstr", &["x"], Arc::clone(&gone));
    c.that(reader.is_some(), "findstr starts");
    let began = Instant::now();
    c.eq(piped.stop(Duration::from_secs(5)), crate::child::Stopped::Exited, "one that reads its stdin exits when the stop closes it");
    c.that(began.elapsed() < Duration::from_secs(3), format!("and without waiting out the bound: {:?}", began.elapsed()));
    let _ = reader.map(|r| r.join());
    c.that(!gone.load(Ordering::SeqCst), "the end of the run the stop retired is not reported as the child gone");
    c.that(piped.stdin().is_none(), "and nothing is left to write to");

    // `ping` never reads its stdin, like a child that hangs.
    let gone = Arc::new(AtomicBool::new(false));
    let reader = run(&piped, "ping", &["-n", "30", "127.0.0.1"], Arc::clone(&gone));
    let began = Instant::now();
    c.eq(piped.stop(Duration::from_millis(500)), crate::child::Stopped::Killed, "one that does not exit is killed after the wait");
    c.that(began.elapsed() < Duration::from_secs(3), format!("so the stop is bounded: {:?}", began.elapsed()));
    let _ = reader.map(|r| r.join());
    c.that(!gone.load(Ordering::SeqCst), "and its end is not reported either");

    // A child that exits by itself is the current run, so its end is reported.
    let gone = Arc::new(AtomicBool::new(false));
    let reader = run(&piped, "cmd", &["/c", "exit", "7"], Arc::clone(&gone));
    let code = reader.and_then(|r| r.join().ok()).flatten();
    c.eq(code, Some(7), "a child that exits on its own is reaped with its code");
    c.that(gone.load(Ordering::SeqCst), "and reported gone, since nothing replaced it");
    c.eq(piped.stop(Duration::from_millis(10)), crate::child::Stopped::Idle, "after which there is nothing to stop");
}

/// Item 1: the list of packs is what `vrstt packs` found installed and whole, and nothing else.
fn the_packs_offered_are_the_ones_the_service_verified(c: &mut Checks) {
    let ok = br#"{"ev":"packs","packs":[{"id":"sense-voice-zh-en-ja-ko-yue-int8-2024-07-17","ok":true}]}"#;
    c.eq(
        stt::packs_of(ok).map(|p| p.into_iter().map(|p| p.id).collect::<Vec<_>>()),
        Ok(vec!["sense-voice-zh-en-ja-ko-yue-int8-2024-07-17".to_string()]),
        "an installed pack that verifies is offered",
    );
    let missing = br#"{"ev":"packs","packs":[{"id":"p","ok":false,"why":"model file missing: x"}]}"#;
    c.that(stt::packs_of(missing).is_err_and(|w| w.contains("missing")), "one that is not installed is not, and the reason is said");
    c.that(stt::packs_of(br#"{"ev":"error","kind":"model","message":"m"}"#).is_err_and(|w| w.contains('m')), "a service that cannot read its manifest says so");
    c.that(stt::packs_of(b"not json").is_err(), "and a line that is not JSON is an error, not an empty list");
    c.eq(crate::settings::DEFAULT_PACK, "sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", "the default is the one pack vrstt is built with");

    c.that(SETTINGS_PAGE.contains("invoke(\"stt_packs\")"), "the settings window asks for the list");
    c.that(!SETTINGS_PAGE.contains("const SENSE_VOICE"), "and does not have one of its own");
    c.that(!SETTINGS_PAGE.contains("現在改了不會馬上生效"), "and no longer says the models are not applied");
}

/// Item 2: an edit is written by the scan's own rules, or not at all, and only to a user's character.
fn a_users_profile_is_written_by_the_scans_rules(c: &mut Checks) {
    let home = make_home("edit");
    let (_, roster) = roster_at(&home.root);
    let file = home.chars.join("good").join(cast::PROFILE);
    let edit = |greeting: &str| cast::Edit {
        name: "阿好".into(),
        outfit: "便服".into(),
        persona: "好人，講話很短。".into(),
        greeting: greeting.into(),
        fillers: vec!["嗯……".into(), "等我一下".into()],
    };

    let saved = roster.save_profile("good", edit("今天也來了啊。"));
    c.that(saved.is_ok(), format!("a user character's profile is saved: {saved:?}"));
    let written = cast::Profile::parse(&std::fs::read(&file).unwrap_or_default());
    c.that(written.as_ref().is_ok_and(|p| p.greeting.as_deref() == Some("今天也來了啊。")), format!("and the file reads back by the scan's rules: {written:?}"));
    c.that(written.as_ref().is_ok_and(|p| p.voice.as_deref() == Some("voice.wav")), "with her voice kept");
    c.that(written.as_ref().is_ok_and(|p| p.fillers.len() == 2), "and her fillers");
    c.that(!home.chars.join("good").join("character.json.new").exists(), "nothing is left beside it");
    c.eq(roster.found().ok().and_then(|f| f.find("good").and_then(|g| g.profile.greeting.clone())), Some("你好。".to_string()), "the roster holds the old one until it scans");
    roster.rescan(&bundle_on_disk);
    c.eq(roster.found().ok().and_then(|f| f.find("good").and_then(|g| g.profile.greeting.clone())), Some("今天也來了啊。".to_string()), "and the new one after");

    let before = std::fs::read(&file).unwrap_or_default();
    for (what, bad) in [
        ("a bracket in the greeting", cast::Edit { greeting: "嗨[wave]".into(), ..edit("") }),
        ("a blank name", cast::Edit { name: "  ".into(), ..edit("") }),
        ("thirteen fillers", cast::Edit { fillers: vec!["嗯".into(); 13], ..edit("") }),
        ("a name with a newline", cast::Edit { name: "阿\n好".into(), ..edit("") }),
    ] {
        let refused = roster.save_profile("good", bad);
        c.that(refused.is_err(), format!("{what} is refused: {refused:?}"));
        c.eq(std::fs::read(&file).unwrap_or_default(), before.clone(), format!("and after {what} the file is as it was"));
    }
    let blank = roster.save_profile("good", edit(""));
    c.that(blank.is_ok(), "an empty greeting is no greeting");
    c.that(cast::Profile::parse(&std::fs::read(&file).unwrap_or_default()).is_ok_and(|p| p.greeting.is_none()), "and is written as none");

    let bundled = roster.save_profile("char-20261107", edit("嗨。"));
    c.that(bundled.as_ref().is_err_and(|w| w.contains("內建")), format!("a bundled character is refused, saying so: {bundled:?}"));
    c.that(roster.save_profile("nobody", edit("嗨。")).is_err(), "and one that is not there");

    let cmd = source_block(MAIN, "fn save_profile(");
    let saved = cmd.find("roster.save_profile(&id, profile)?;");
    let scanned = cmd.find("roster.rescan(");
    let told = cmd.find("tell_and_reload(&app, &core);");
    c.that(matches!((saved, scanned, told), (Some(a), Some(b), Some(t)) if a < b && b < t), "the command saves, scans again, then reloads the pet");
    let page = source_block(SETTINGS_PAGE, "async function saveProfile(");
    c.that(page.contains("invoke(\"save_profile\""), "the editor saves through it");
}

/// Item 2: a bundled character copied whole into a new folder, which the scan then accepts.
fn a_bundled_character_is_copied_to_a_folder_of_her_own(c: &mut Checks) {
    let home = make_home("copy");
    let (store, roster) = roster_at(&home.root);
    let copied = roster.copy(&bundle_on_disk, "char-20261107", "mine");
    c.eq(copied.as_ref().map(|id| id.to_string()), Ok("mine".to_string()), "the bundled character is copied under the new id");
    let art: Value = serde_json::from_slice(&bundle_on_disk("characters/char-20261107/layers.json").unwrap_or_default()).unwrap_or(Value::Null);
    let images: Vec<String> = art["layers"].as_array().into_iter().flatten().filter_map(|l| l["file"].as_str().map(String::from)).collect();
    c.that(images.len() >= 10, format!("her art names its images: {}", images.len()));
    for file in images.iter().map(String::as_str).chain([cast::PROFILE, cast::RIG, cast::ART]) {
        let same = std::fs::read(home.chars.join("mine").join(file)).ok() == bundle_on_disk(&format!("characters/char-20261107/{file}"));
        c.that(same, format!("{file} is copied byte for byte"));
    }
    roster.rescan(&bundle_on_disk);
    let mine = roster.found().ok().and_then(|f| f.find("mine").cloned());
    c.that(mine.as_ref().is_some_and(|m| m.source == cast::Source::User && m.profile.name == "小克"), format!("and the scan takes her as the user's own: {mine:?}"));
    c.that(roster.choose("mine", &store).is_ok(), "she can be chosen");
    c.that(roster.save_profile("mine", cast::Edit { name: "我的小毛".into(), outfit: String::new(), persona: "y".into(), greeting: String::new(), fillers: vec![] }).is_ok(), "and edited, which the bundled one cannot be");

    let made = std::fs::read_dir(&home.chars).map(|d| d.count()).unwrap_or(0);
    for (to, what) in [("mine", "an id that is already a character"), ("char-20261107", "a bundled id"), ("Bad Name", "an id the folder rule refuses"), ("big", "a folder already there, even a refused one")] {
        let r = roster.copy(&bundle_on_disk, "char-20261107", to);
        c.that(r.is_err(), format!("{what} is refused: {r:?}"));
    }
    let _ = std::fs::create_dir_all(home.chars.join("plain"));
    put(&home.chars.join("plain"), "keep.txt", b"mine");
    let r = roster.copy(&bundle_on_disk, "char-20261107", "plain");
    c.that(r.as_ref().is_err_and(|w| w.contains("已經存在")), format!("a folder the scan does not list is refused too: {r:?}"));
    c.eq(std::fs::read(home.chars.join("plain").join("keep.txt")).ok(), Some(b"mine".to_vec()), "and nothing in it is touched");
    c.eq(std::fs::read_dir(&home.chars).map(|d| d.count()).unwrap_or(0), made + 1, "no refused copy leaves a folder behind");
    c.that(roster.copy(&bundle_on_disk, "good", "good2").is_err(), "a user character is not copied; she is already the user's");

    let cmd = source_block(MAIN, "fn copy_character(");
    let copy = cmd.find("roster.copy(&read, &from, &to)?;");
    let scan = cmd.find("roster.rescan(&read);");
    let choose = cmd.find("roster.choose(id.as_str(), &store)?;");
    let reload = cmd.find("tell_and_reload(&app, &core);");
    c.that(
        matches!((copy, scan, choose, reload), (Some(a), Some(b), Some(d), Some(e)) if a < b && b < d && d < e),
        "the command copies, scans, makes the copy hers, then reloads the pet",
    );
}

/// Item 3: a real shortcut in a Startup folder of the test's own, read back through Windows.
fn autostart_is_a_startup_shortcut_that_runs_start_pet(c: &mut Checks) {
    let home = temp_home("startup");
    let repo = std::env::current_exe().ok().as_deref().and_then(crate::autostart::repo_of);
    c.that(repo.as_ref().is_some_and(|r| r.join("Scripts").join("start_pet.ps1").is_file()), format!("the repo is found above the executable: {repo:?}"));
    let on = crate::autostart::Autostart::new(Some(home.0.clone()), repo.clone());
    let link = home.0.join(crate::autostart::LINK);
    c.that(!on.is_on(), "with no shortcut the switch is off");

    let made = on.set(true);
    c.that(made.is_ok() && link.is_file(), format!("switched on, the shortcut is made: {made:?}"));
    c.that(on.is_on(), "and the switch reads on");
    let want = repo.as_deref().map(crate::autostart::shortcut);
    let read = crate::autostart::read(&link);
    // Compared without case: `SystemRoot` says `C:\WINDOWS`, and the shortcut reads back `C:\Windows`.
    let lower = |p: &PathBuf| p.display().to_string().to_lowercase();
    c.eq(read.as_ref().ok().map(|s| lower(&s.target)), want.as_ref().map(|s| lower(&s.target)), "it runs Windows PowerShell");
    c.eq(read.as_ref().ok().map(|s| s.arguments.clone()), want.as_ref().map(|s| s.arguments.clone()), "with start_pet.ps1's arguments");
    c.eq(read.as_ref().ok().map(|s| s.working_dir.clone()), repo.clone(), "in the repo");
    let script = include_str!("../../../Scripts/start_pet.ps1");
    c.that(
        script.contains("$s.Arguments = \"-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `\"$PSCommandPath`\"\""),
        "the desktop shortcut start_pet.ps1 makes runs the same arguments",
    );
    c.that(
        want.as_ref().is_some_and(|s| s.arguments == format!("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\\Scripts\\start_pet.ps1\"", repo.as_deref().map(|r| r.display().to_string()).unwrap_or_default())),
        "with the script's path where $PSCommandPath is",
    );
    c.that(on.set(true).is_ok() && on.is_on(), "switching on again leaves it on");
    c.that(on.set(false).is_ok() && !link.exists(), "switched off, it is deleted");
    c.that(on.set(false).is_ok(), "and switching off again is nothing to do");

    // The file's word is not the switch: the shortcut is.
    let store = settings::Store::open(Some(home.0.clone()));
    let mut request = store.current().to_value();
    request["switches"]["autostart"] = json!(true);
    let saved = store.set_with(&request, |next| on.set(next.switches.autostart));
    c.that(saved.is_ok() && on.is_on(), "a request that turns it on makes the shortcut");
    let _ = std::fs::remove_file(&link);
    c.that(!on.overlay(store.loaded()).settings.switches.autostart, "a shortcut deleted by hand reads as off, whatever the file says");
    let nowhere = crate::autostart::Autostart::new(None, repo);
    let before = std::fs::read(home.0.join(settings::FILE)).unwrap_or_default();
    request["volume"] = json!(0.1);
    let refused = store.set_with(&request, |next| nowhere.set(next.switches.autostart));
    c.that(refused.is_err(), "a shortcut that cannot be made refuses the request");
    c.eq(std::fs::read(home.0.join(settings::FILE)).unwrap_or_default(), before, "and nothing of it is saved");

    c.that(crate::autostart::startup_dir().is_some_and(|d| d.ends_with(r"Microsoft\Windows\Start Menu\Programs\Startup") || std::env::var_os(crate::autostart::DIR_ENV).is_some()), "the real folder is the Startup folder under APPDATA");
    let get = source_block(MAIN, "fn get_settings(");
    c.that(get.contains("autostart.overlay(store.loaded())"), "the window is shown the shortcut's state");
    let set = source_block(MAIN, "async fn set_settings(");
    c.that(set.contains("autostart.set(on)") && set.contains("Ok(autostart.overlay(after))"), "and a request changes the shortcut and answers with it");
}

/// Item 4a: grown near an edge, the window stays on the screen's work area.
fn the_window_stays_inside_the_work_area(c: &mut Checks) {
    // A 1920x1080 screen at 1.25 with a 48 px taskbar along the bottom.
    let work = Some(((0, 0), (1920, 1032)));
    let size = (575, 775);
    let near_top = (100, 100);
    let (to, big) = settings::window_rect(near_top, size, 1.2, 1.25, work);
    c.eq(big, (690, 930), "at 1.2 she is 1.2 times");
    c.that(to.1 >= 0, format!("grown near the top, the window does not go above it: {to:?}"));
    c.eq(to.1, 0, "it stops at the top edge");
    let unclamped = settings::window_rect(near_top, size, 1.2, 1.25, None).0;
    c.that(unclamped.1 < 0, format!("which is where it would have gone: {unclamped:?}"));
    let (to, big) = settings::window_rect((1500, 200), size, 1.2, 1.25, work);
    c.that(to.0 + big.0 as i32 <= 1920, format!("near the right edge it moves left to stay on: {to:?} {big:?}"));
    let on_taskbar = (100, 1032 - 775);
    let (to, big) = settings::window_rect(on_taskbar, size, 1.2, 1.25, work);
    c.eq(to.1 + big.1 as i32, 1032, "standing on the taskbar, her feet stay on it");
    let (to, big) = settings::window_rect(near_top, size, 1.6, 2.0, work);
    c.that(big.1 > 1032 && to == (100, 0), format!("taller than the screen, its top-left stays on it: {to:?} {big:?}"));
    let second = Some(((1920, -200), (2560, 1400)));
    let (to, _) = settings::window_rect((2000, -150), size, 1.2, 1.25, second);
    c.eq(to.1, -200, "on a monitor above the first, the work area's own top is the edge");
    c.that(source_block(MAIN, "fn fit_window(").contains("m.work_area()"), "the window is placed with its monitor's work area");
}

/// Item 4b: the settings window is above the always-on-top pet while it is being used.
fn the_settings_window_comes_above_the_pet(c: &mut Checks) {
    let open = source_block(MAIN, "async fn open_settings(");
    c.that(open.contains(".always_on_top(true)"), "it opens above every window, the pet included");
    c.that(
        open.contains("tauri::WindowEvent::Focused(focused)") && open.contains("handle.set_always_on_top(*focused)"),
        "and stays above while it has the focus, an ordinary window when it does not",
    );
    let reopened = open.find("win.set_always_on_top(true);");
    let focused = open.find("return win.set_focus()");
    c.that(matches!((reopened, focused), (Some(a), Some(b)) if a < b), "opened again, it comes back above her");
}

// ---------------------------------------------------------------------------------------------
// pet_themes (Docs/DESIGN_Themes.md)

use crate::themes;

/// The bundled 紙本's file, which every case below varies one field of.
fn paper_theme() -> Value {
    json!({
        "name": "紙本",
        "colors": { "accent": "#d97757", "paper": "#f3f2ef", "ink": "#2b2b2b", "border": "#cfc8bc", "controls": "#f3f2ef" },
        "radius": 18,
        "icon": "icon.svg",
        "paper_texture": "paper.svg"
    })
}

fn theme_of(value: &Value) -> Result<themes::ThemeFile, String> {
    themes::ThemeFile::parse(&serde_json::to_vec(value).unwrap_or_default())
}

fn a_theme_file_keeps_to_its_rules(c: &mut Checks) {
    c.that(theme_of(&paper_theme()).is_ok(), "the bundled 紙本's file is a theme");
    let mut bare = paper_theme();
    if let Some(o) = bare.as_object_mut() {
        o.remove("icon");
        o.remove("paper_texture");
    }
    c.that(theme_of(&bare).is_ok(), "the two pictures are optional");
    let upper = theme_of(&one_field_over(&paper_theme(), "colors", json!({ "accent": "#D97757", "paper": "#F3F2EF", "ink": "#2B2B2B", "border": "#CFC8BC", "controls": "#F3F2EF" })));
    c.eq(upper.map(|t| String::from(t.colors.accent)), Ok("#d97757".to_string()), "a colour in capitals is read, and kept in lower case");
    for radius in [0, 28] {
        c.that(theme_of(&one_field_over(&paper_theme(), "radius", json!(radius))).is_ok(), format!("radius {radius} is in range"));
    }

    let colour = |value: Value| {
        let mut t = paper_theme();
        t["colors"]["ink"] = value;
        t
    };
    let refused: Vec<(&str, Value, &str)> = vec![
        ("an unknown field", one_field_over(&paper_theme(), "css", json!("body{}")), "css"),
        ("an unknown colour", { let mut t = paper_theme(); t["colors"]["link"] = json!("#000000"); t }, "link"),
        ("a missing colour", { let mut t = paper_theme(); if let Some(o) = t["colors"].as_object_mut() { o.remove("border"); } t }, "border"),
        ("a three-digit colour", colour(json!("#fff")), "#rrggbb"),
        ("a named colour", colour(json!("red")), "#rrggbb"),
        ("a colour with a declaration after it", colour(json!("#000000; background: url(x)")), "#rrggbb"),
        ("a colour that is not hex", colour(json!("#12345g")), "#rrggbb"),
        ("radius 29", one_field_over(&paper_theme(), "radius", json!(29)), "radius"),
        ("a negative radius", one_field_over(&paper_theme(), "radius", json!(-1)), "radius"),
        ("a fractional radius", one_field_over(&paper_theme(), "radius", json!(4.5)), "radius"),
        ("a blank name", one_field_over(&paper_theme(), "name", json!("  ")), "name"),
        ("a 33-character name", one_field_over(&paper_theme(), "name", json!("夜".repeat(33))), "name"),
        ("a name with a newline", one_field_over(&paper_theme(), "name", json!("夜\n色")), "name"),
        ("an icon outside the folder", one_field_over(&paper_theme(), "icon", json!("../icon.svg")), "icon"),
        ("an icon with a quote", one_field_over(&paper_theme(), "icon", json!("a\").svg")), "icon"),
        ("an icon with a space", one_field_over(&paper_theme(), "icon", json!("my icon.svg")), "icon"),
        ("an icon in capitals", one_field_over(&paper_theme(), "icon", json!("Icon.svg")), "icon"),
        ("a gif", one_field_over(&paper_theme(), "paper_texture", json!("paper.gif")), "paper_texture"),
        ("theme.json as the icon", one_field_over(&paper_theme(), "icon", json!("theme.json")), "icon"),
    ];
    for (what, value, says) in refused {
        let parsed = theme_of(&value);
        c.that(parsed.as_ref().is_err_and(|w| w.contains(says)), format!("{what} is refused, saying {says}: {parsed:?}"));
    }
}

/// A user's themes folder with one theme of each kind the scan must tell apart.
struct ThemeHome {
    root: PathBuf,
    dir: PathBuf,
    skipped: Vec<String>,
}

impl Drop for ThemeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn theme_folder(dir: &Path, file: &Value) {
    put(dir, themes::FILE, &serde_json::to_vec_pretty(file).unwrap_or_default());
    put(dir, "icon.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
    put(dir, "paper.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
}

fn make_theme_home(tag: &str) -> ThemeHome {
    let root = std::env::temp_dir().join(format!("vrpet-themes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join(themes::USER_DIR);
    let mut skipped = Vec::new();

    let good = dir.join("dusk");
    let mut dusk = one_field_over(&paper_theme(), "name", json!("黃昏"));
    dusk["icon"] = json!("moon.png");
    theme_folder(&good, &dusk);
    put(&good, "moon.png", b"\x89PNG\r\n\x1a\n");
    put(&good, "notes.txt", b"not served");
    put(&good, "README.md", b"a file nothing reads is allowed");

    theme_folder(&dir.join("Bad Name"), &paper_theme());
    theme_folder(&dir.join("paper"), &paper_theme());
    theme_folder(&dir.join("noicon"), &one_field_over(&paper_theme(), "icon", json!("star.svg")));
    let _ = std::fs::create_dir_all(dir.join("nofile"));
    put(&dir.join("nofile"), "icon.svg", b"<svg/>");
    theme_folder(&dir.join("bigimage"), &paper_theme());
    sized(&dir.join("bigimage").join("paper.svg"), themes::IMAGE_CAP + 1);
    theme_folder(&dir.join("bigjson"), &paper_theme());
    sized(&dir.join("bigjson").join(themes::FILE), themes::JSON_CAP + 1);
    theme_folder(&dir.join("huge"), &paper_theme());
    sized(&dir.join("huge").join("blob.bin"), themes::TOTAL_CAP + 1);
    theme_folder(&dir.join("css"), &one_field_over(&paper_theme(), "css", json!("body { display: none }")));
    put(&dir, "README.txt", b"a file beside the folders is not a theme");

    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(dir.join("linked")).arg(&good).output();
        match made {
            Ok(out) if out.status.success() => {}
            Ok(out) => skipped.push(format!("junction: {}", String::from_utf8_lossy(&out.stdout).trim())),
            Err(e) => skipped.push(format!("junction: {e}")),
        }
    }
    ThemeHome { root, dir, skipped }
}

fn every_user_theme_folder_is_loaded_or_says_why(c: &mut Checks) {
    let home = make_theme_home("scan");
    for why in &home.skipped {
        eprintln!("pet_themes: a link check is skipped because the OS refused to make the link: {why}");
    }
    let bundled = ["paper", "night"].map(|id| cast::Id::parse(id).expect("a valid id"));
    let (found, refused) = themes::scan(&home.dir, &bundled);
    c.eq(found.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["dusk"], "the one good folder is the one loaded");
    if let Some(dusk) = found.first() {
        let listed = dusk.listed();
        c.eq(listed.name.as_str(), "黃昏", "with its own name");
        c.eq(listed.icon, Some(format!("{}dusk/moon.png", themes::ORIGIN)), "and its pictures drawn through the scheme");
        c.eq(listed.paper_texture, Some(format!("{}dusk/paper.svg", themes::ORIGIN)), "both of them");
    }
    let why = |id: &str| refused.iter().find(|r| r.id == id).map(|r| r.why.clone());
    let mut expect = vec![
        ("Bad Name", "不合規則"),
        ("paper", "內建主題"),
        ("noicon", "star.svg"),
        ("nofile", "缺 theme.json"),
        ("bigimage", "超過 2 MB"),
        ("bigjson", "超過 64 KB"),
        ("huge", "整個主題"),
        ("css", "css"),
    ];
    if !home.skipped.iter().any(|s| s.starts_with("junction")) {
        expect.push(("linked", "捷徑"));
    }
    for (id, part) in &expect {
        match why(id) {
            Some(w) => c.that(w.contains(part), format!("{id} is listed and says why ({part}): {w}")),
            None => c.that(false, format!("{id} is listed as refused")),
        }
    }
    c.eq(refused.len(), expect.len(), "and nothing else is listed, not even the file beside them");
    c.eq(themes::scan(&home.dir.join("absent"), &bundled).0.len(), 0, "a home with no themes folder has none");
}

fn the_bundle_ships_paper_and_night(c: &mut Checks) {
    let found = themes::Found::load(&bundle_on_disk, None);
    c.that(found.is_ok(), format!("the bundle's themes load: {:?}", found.as_ref().err()));
    let Ok(found) = found else { return };
    c.eq(found.refused.len(), 0, "every bundled theme can be shown");
    let names: Vec<(&str, &str)> = found.themes.iter().map(|t| (t.id.as_str(), t.file.name.as_str())).collect();
    c.eq(names, vec![("paper", "紙本"), ("night", "夜色")], "紙本 and 夜色, in the index's order");
    c.eq(found.default.as_str(), themes::DEFAULT, "the index's default is the one a fresh pet is set to");
    c.eq(Settings::default().look.theme.as_str(), themes::DEFAULT, "and the settings' default names it");
    if let Some(paper) = found.find("paper") {
        c.eq(Some(paper.file.clone()), theme_of(&paper_theme()).ok(), "紙本 is today's look");
        c.eq(paper.listed().icon.as_deref(), Some("themes/paper/icon.svg"), "and a bundled picture is the page's own file");
        // The stylesheet starts at 紙本, so the page before Rust answers is the page after.
        let root = css_rule(PAGE, ":root").unwrap_or_default();
        for (role, value) in [
            ("accent", &paper.file.colors.accent),
            ("paper", &paper.file.colors.paper),
            ("ink", &paper.file.colors.ink),
            ("border", &paper.file.colors.border),
            ("controls", &paper.file.colors.controls),
        ] {
            let want = format!("--{role}: {};", String::from(value.clone()));
            c.that(root.contains(&want), format!("the page's :root starts {want}"));
        }
        c.that(root.contains(&format!("--radius: {}px;", paper.file.radius)), "and the radius");
    }
    if let Some(night) = found.find("night") {
        let dark = |c: &themes::Color| {
            let hex = String::from(c.clone());
            u32::from_str_radix(&hex[1..], 16).map(|v| ((v >> 16) & 255) + ((v >> 8) & 255) + (v & 255)).unwrap_or(0)
        };
        c.that(dark(&night.file.colors.paper) < dark(&night.file.colors.ink), "夜色 is light ink on dark paper, so the switch is seen");
    }
}

fn a_missing_theme_falls_back_and_says_why(c: &mut Checks) {
    let home = make_theme_home("pick");
    let found = themes::Found::load(&bundle_on_disk, Some(&home.dir));
    let Ok(found) = found else { return c.that(false, "the themes load with user folders beside the bundle") };
    let id = |s: &str| cast::Id::parse(s).expect("a valid id");
    let (shown, why) = found.pick(&id("dusk"));
    c.that(shown.id.as_str() == "dusk" && why.is_none(), "a user theme that loaded is shown as chosen");
    let (shown, why) = found.pick(&id("gone"));
    c.that(shown.id.as_str() == themes::DEFAULT && why.as_deref().is_some_and(|w| w.contains("gone") && w.contains("紙本")), format!("one that is not there falls back to 紙本 and says so: {why:?}"));
    let (shown, why) = found.pick(&id("bigimage"));
    c.that(shown.id.as_str() == themes::DEFAULT && why.as_deref().is_some_and(|w| w.contains("超過")), format!("one that was refused says why it was: {why:?}"));

    let store = settings::Store::open(Some(home.root.clone()));
    let mut request = store.current().to_value();
    request["look"]["theme"] = json!("gone");
    let saved = store.set(&request);
    c.that(saved.is_ok(), "the settings themselves take any id, since whether it can be shown is the themes' to say");
    let set = source_block(MAIN, "async fn set_settings(");
    c.that(
        set.contains("next.look.theme != before.look.theme && looks.found()?.find(next.look.theme.as_str()).is_none()"),
        "the settings window cannot move to a theme the scan cannot show, and keeping one is never refused",
    );
}

fn the_theme_scheme_serves_only_checked_files(c: &mut Checks) {
    let home = make_theme_home("scheme");
    let looks = themes::Themes::load(&bundle_on_disk, Some(home.root.clone()));
    let served = |path: &str| looks.serve(path);
    c.that(served("/dusk/theme.json").is_ok_and(|(_, k)| k == themes::Kind::Json), "theme.json is served as JSON");
    c.that(served("/dusk/moon.png").is_ok_and(|(b, k)| k == themes::Kind::Png && b.starts_with(b"\x89PNG")), "a PNG is served with its bytes");
    c.that(served("/dusk/paper.svg").is_ok_and(|(_, k)| k == themes::Kind::Svg), "an SVG is served");
    c.eq(themes::Kind::Svg.mime(), "image/svg+xml", "as an image");

    for path in ["/dusk/../css/theme.json", "/dusk/..%2Fcss%2Ftheme.json", "/dusk/notes.txt", "/dusk/README.md", "/dusk/moon.png:x.png", "/dusk/Moon.png", "/dusk/a%22).svg", "/dusk/", "dusk/theme.json", "/dusk/%ZZ.svg"] {
        match served(path) {
            Err(cast::Refusal::Forbidden(_)) => c.that(true, ""),
            other => c.that(false, format!("{path} is refused as forbidden, saw {other:?}")),
        }
    }
    for path in ["/nobody/theme.json", "/paper/theme.json", "/bigimage/theme.json", "/Bad%20Name/theme.json", "/dusk/gone.svg"] {
        match served(path) {
            Err(cast::Refusal::Missing(_)) => c.that(true, ""),
            other => c.that(false, format!("{path} is not found, saw {other:?}")),
        }
    }
    sized(&home.dir.join("dusk").join("paper.svg"), themes::IMAGE_CAP + 1);
    c.that(
        matches!(served("/dusk/paper.svg"), Err(cast::Refusal::Forbidden(ref w)) if w.contains("超過")),
        "a picture over its cap after the scan is refused when it is asked for",
    );

    c.that(themes::ORIGIN.contains(themes::SCHEME), "the origin the page uses is the scheme Rust registers");
    c.that(MAIN.contains("register_asynchronous_uri_scheme_protocol(themes::SCHEME"), "Rust registers it");
    c.that(MAIN.contains("user_file(&label, themes::SCHEME, &request, serve)"), "behind the per-window rule");
    let user_file = source_block(MAIN, "fn user_file(");
    for header in [
        ".header(\"Content-Security-Policy\", \"default-src 'none'; style-src 'unsafe-inline'\")",
        ".header(\"X-Content-Type-Options\", \"nosniff\")",
    ] {
        c.that(user_file.contains(header), format!("a user's file is answered with {header}"));
    }
}

/// The page maps a theme's values onto its own custom properties and takes no CSS from it: every
/// colour on the dialogue is one of the five or mixed from them, each value is checked again before
/// it is written, and nothing a theme gives can add a rule.
fn the_page_takes_values_and_never_css(c: &mut Checks) {
    let apply = page_block("function applyTheme(");
    c.that(apply.contains("for (const role of [\"accent\", \"paper\", \"ink\", \"border\", \"controls\"])"), "applyTheme writes the five roles");
    c.that(apply.contains("if (THEME_COLOR.test(c)) root.style.setProperty(\"--\" + role, c);"), "each only when it is #rrggbb");
    c.that(apply.contains("t.radius >= 0 && t.radius <= 28"), "the radius only in its range");
    c.that(apply.contains("THEME_URL.test(url) ? 'url(\"' + url + '\")' : \"none\""), "and a picture only from the two places themes are served");
    c.that(PAGE.contains("const THEME_COLOR = /^#[0-9a-f]{6}$/;"), "the colour rule is Rust's");
    for needle in ["cssText", "insertRule", "createElement(\"style\")", "createElement(\"link\")", ".innerHTML"] {
        c.that(!PAGE.contains(needle) && !SETTINGS_PAGE.contains(needle), format!("neither page uses {needle}"));
    }
    let cap = apply.split_once("t.radius <= ").and_then(|(_, rest)| rest.split(')').next()?.trim().parse::<u8>().ok());
    c.eq(cap, Some(themes::RADIUS_MAX), "the page's radius cap is Rust's");

    // The dialogue's rules name none of 紙本's colours any more, only the properties.
    let css = PAGE.split("</style>").next().unwrap_or("");
    let rules = css.split_once(":root.no-icon").map_or("", |(_, rest)| rest);
    let dialogue: String = rules.split("\n  }").filter(|r| r.contains(".chat-") || r.contains("#controls") || r.contains("#gate")).collect();
    for literal in ["#cfc8bc", "#f3f2ef", "#2b2b2b", "#d97757", "#8a4a33", "#a39d93", "#fbfaf8", "rgba(243, 242, 239"] {
        c.that(!dialogue.contains(literal), format!("the dialogue's rules no longer name {literal}"));
    }
    for property in ["--accent", "--paper", "--ink", "--border", "--controls", "--radius", "--icon", "--paper-texture", "--name", "--faint"] {
        c.that(dialogue.contains(&format!("var({property})")) || css.contains(&format!("var({property})")), format!("{property} is read"));
    }
}

/// Choosing a theme in the settings window applies it to the pet without reloading her.
fn a_switch_applies_without_reloading(c: &mut Checks) {
    let apply = page_block("function applySettings(");
    c.that(apply.contains("if (s.look.theme !== themeAsked) loadTheme(s.look.theme);"), "a settings change that names another theme loads it");
    let load = page_block("function loadTheme(");
    c.that(load.contains("invoke(\"themes\")") && load.contains("applyTheme(list.shown);"), "from Rust's list, the one it can show");
    c.that(load.contains("if (themeAsked !== id) return;"), "and an answer for a theme since replaced is dropped");
    c.that(!source_block(MAIN, "async fn set_settings(").contains("reload"), "set_settings reloads nothing");
    c.that(source_block(MAIN, "fn themes(").contains("looks.rescan("), "and each list is a fresh scan");

    let draw = source_block(SETTINGS_PAGE, "async function drawThemes(");
    c.that(draw.contains("change((s) => (s.look.theme = t.id));"), "the settings window chooses through the form");
    c.that(draw.contains("list.refused.map((r) => el(\"div\", \"cast-item off\", r.id + \"：\" + r.why))"), "and lists every refused folder with its reason");
    c.that(draw.contains("fallback.textContent = list.fallback"), "and says when the chosen one fell back");
    c.that(SETTINGS_PAGE.contains(">打開主題資料夾</button>"), "with a button that opens the folder");
    let open = source_block(MAIN, "fn open_themes_folder(");
    c.that(open.contains("themes::dir(&home::dir()?)?") && open.contains("Command::new(\"explorer\").arg(&dir)"), "which opens <home>/themes in Explorer");
    let home = temp_home("themes-dir");
    c.eq(themes::dir(&home.0).ok(), Some(home.0.join(themes::USER_DIR)), "making it first");
    c.that(SETTINGS_PAGE.contains("window.addEventListener(\"focus\", () => {"), "and the list is scanned again when the window comes back");
}
