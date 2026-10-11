use super::*;

pub(super) fn profile_matches(profile: &ConnectionProfile, query: &str) -> bool {
    let text = format!(
        "{} {} {} {}",
        profile.name,
        profile_summary(profile),
        profile.group.as_deref().unwrap_or_default(),
        profile.auth.username().unwrap_or_default()
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|part| text.contains(part))
}

pub(super) fn profile_index_for_row(
    document: &ProfilesDocument,
    row: &ListBoxRow,
) -> Option<usize> {
    let name = row.widget_name();
    let id = name.strip_prefix("profile:")?;
    document
        .profiles
        .iter()
        .position(|profile| profile.id.to_string() == id)
}

pub(super) fn profile_for_row(
    document: &ProfilesDocument,
    row: &ListBoxRow,
) -> Option<ConnectionProfile> {
    profile_index_for_row(document, row).map(|index| document.profiles[index].clone())
}

pub(super) fn refresh_profile_list_for_state(state: &AppState) {
    refresh_profile_list_internal(
        &state.profiles_list,
        &state.profiles_doc.borrow(),
        Some(state),
    );
}

pub(super) fn refresh_profile_list_internal(
    list: &ListBox,
    document: &ProfilesDocument,
    state: Option<&AppState>,
) {
    let selected = list.selected_row().map(|row| row.widget_name().to_string());
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let query = state
        .map(|state| state.chrome.profile_search.text().to_string())
        .unwrap_or_default();
    let language = state
        .map(|state| state.app_settings.borrow().language.clone())
        .unwrap_or_default();
    let mut groups: Vec<(&str, Vec<&ConnectionProfile>)> = Vec::new();
    for profile in &document.profiles {
        if matches!(profile.protocol, ProtocolKind::LocalShell) || !profile_matches(profile, &query)
        {
            continue;
        }
        let name = profile.group.as_deref().unwrap_or_default().trim();
        if let Some((_, profiles)) = groups.iter_mut().find(|(group, _)| *group == name) {
            profiles.push(profile);
        } else {
            groups.push((name, vec![profile]));
        }
    }
    let count: usize = groups.iter().map(|(_, profiles)| profiles.len()).sum();
    let show_groups = groups.len() > 1 || groups.first().is_some_and(|(name, _)| !name.is_empty());
    for (group, profiles) in groups {
        if show_groups {
            let row = ListBoxRow::new();
            row.set_selectable(false);
            row.set_activatable(false);
            row.add_css_class("profile-group-row");
            let heading = GtkBox::new(Orientation::Horizontal, 6);
            let name = if group.is_empty() {
                tr(&language, "未分组", "Ungrouped")
            } else {
                group
            };
            let label = Label::new(Some(name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_width_chars(1);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_tooltip_text(Some(name));
            heading.append(&label);
            heading.append(&Label::new(Some(&profiles.len().to_string())));
            row.set_child(Some(&heading));
            list.append(&row);
        }
        for profile in profiles {
            let row = make_profile_row(profile);
            list.append(&row);
            if selected.as_deref() == Some(row.widget_name().as_str()) {
                list.select_row(Some(&row));
            }
        }
    }
    if let Some(state) = state {
        state.chrome.empty_profiles.set_visible(count == 0);
        state
            .chrome
            .empty_profiles
            .set_text(if query.trim().is_empty() {
                tr(
                    &language,
                    "还没有保存的会话。点击 + 添加连接。",
                    "No saved sessions. Use + to add a connection.",
                )
            } else {
                tr(&language, "没有匹配的会话。", "No matching sessions.")
            });
    }
}

fn make_profile_row(profile: &ConnectionProfile) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_widget_name(&format!("profile:{}", profile.id));
    row.add_css_class("saved-profile-row");
    row.set_tooltip_text(Some(&format!(
        "{}\n{}",
        profile.name,
        profile_summary(profile)
    )));
    let content = GtkBox::new(Orientation::Horizontal, 9);
    let icon = Image::from_icon_name(match profile.protocol {
        ProtocolKind::Serial => "drive-harddisk-symbolic",
        ProtocolKind::Vnc => "video-display-symbolic",
        _ => "network-server-symbolic",
    });
    icon.set_pixel_size(16);
    icon.add_css_class("profile-protocol-icon");
    let text = GtkBox::new(Orientation::Vertical, 1);
    text.set_hexpand(true);
    text.set_size_request(0, -1);
    let title = Label::new(Some(&profile.name));
    title.add_css_class("profile-name");
    let subtitle = Label::new(Some(&profile_summary(profile)));
    subtitle.add_css_class("profile-detail");
    for label in [&title, &subtitle] {
        label.set_xalign(0.0);
        label.set_width_chars(1);
        label.set_hexpand(true);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        text.append(label);
    }
    content.append(&icon);
    content.append(&text);
    row.set_child(Some(&content));
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_names_hosts_groups_and_usernames_without_case_sensitivity() {
        let mut profile = ConnectionProfile::new("API Production", ProtocolKind::Ssh);
        profile.host = Some("192.0.2.10".into());
        profile.group = Some("生产环境".into());
        profile.auth = AuthConfig::KeyboardInteractive {
            username: "Deploy".into(),
        };
        assert!(profile_matches(&profile, "api deploy"));
        assert!(profile_matches(&profile, "生产 192.0.2"));
        assert!(profile_matches(&profile, "   "));
        assert!(!profile_matches(&profile, "staging"));
    }
}
