//! Rules about conversations, shared by the lists, the tab badges, the menu bar and
//! notifications, so they always agree. No SDK: these are unit tested.

use crate::{
    models::{ArchiveSummary, ListFilter, RoomSummary, Scope, UnreadKind},
    text::normalize_for_search,
};

/// Direct chats live in Chats; everything else (groups, public rooms) in Rooms.
pub(crate) fn scope_of(is_direct: bool) -> Scope {
    if is_direct { Scope::Chats } else { Scope::Rooms }
}

/// The facts that decide unread state, so it can be computed before a summary exists.
pub(crate) struct UnreadFacts {
    pub is_invite: bool,
    pub is_marked_unread: bool,
    pub mention_count: u64,
    pub unread_count: u64,
    pub is_muted: bool,
}

/// Something new the person hasn't silenced, or a conversation they flagged.
pub(crate) fn is_unread(f: &UnreadFacts) -> bool {
    f.is_invite || f.is_marked_unread || f.mention_count > 0 || (f.unread_count > 0 && !f.is_muted)
}

/// A mention shows even when muted: a ping should reach you.
pub(crate) fn unread_kind(f: &UnreadFacts) -> UnreadKind {
    if f.mention_count > 0 {
        UnreadKind::Mention
    } else if is_unread(f) {
        UnreadKind::Unread
    } else {
        UnreadKind::None
    }
}

/// Every word of the query starts a word in the name, address or (for a DM) the other person's
/// ID, in any order; or appears anywhere in them.
pub(crate) fn room_matches_query(room: &RoomSummary, query: &str) -> bool {
    let words: Vec<String> = normalize_for_search(query).split_whitespace().map(str::to_owned).collect();
    if words.is_empty() {
        return true;
    }
    let haystack = normalize_for_search(&format!(
        "{} {} {}",
        room.name,
        room.alias.as_deref().unwrap_or_default(),
        room.dm_user_id.as_deref().unwrap_or_default()
    ));
    let starts: Vec<&str> =
        haystack.split(|c: char| c.is_whitespace() || "#@:._-".contains(c)).filter(|s| !s.is_empty()).collect();
    words.iter().all(|w| starts.iter().any(|s| s.starts_with(w.as_str())) || haystack.contains(w.as_str()))
}

/// One list's conversations, invitations first, then newest activity first. `None` is the unified
/// list: chats and rooms together, as Messages shows them.
pub(crate) fn filter_rooms(
    rooms: &[RoomSummary],
    scope: Option<Scope>,
    filter: ListFilter,
    query: &str,
) -> Vec<RoomSummary> {
    let shown = rooms.iter().filter(|r| {
        scope.is_none_or(|s| r.scope == s)
            && r.is_archived == (filter == ListFilter::Archived)
            && match filter {
                ListFilter::All | ListFilter::Archived => true,
                ListFilter::Unread => r.is_unread,
                ListFilter::Favourites => r.is_favourite,
            }
            && room_matches_query(r, query)
    });
    let (mut invites, rest): (Vec<RoomSummary>, Vec<RoomSummary>) = shown.cloned().partition(|r| r.is_invite);
    invites.extend(rest);
    invites
}

/// Wants attention from the main list: archived conversations wait in Archived instead.
pub(crate) fn counts_toward_badge(room: &RoomSummary) -> bool {
    room.is_unread && !room.is_archived
}

/// How many conversations in a scope (or everywhere) want attention, for its tab badge.
pub(crate) fn unread_total(rooms: &[RoomSummary], scope: Option<Scope>) -> u32 {
    rooms.iter().filter(|r| scope.is_none_or(|s| r.scope == s) && counts_toward_badge(r)).count() as u32
}

/// The archived conversations in a scope (or everywhere), for the row that leads to them.
pub(crate) fn archive_summary(rooms: &[RoomSummary], scope: Option<Scope>) -> ArchiveSummary {
    let archived = rooms.iter().filter(|r| r.is_archived && scope.is_none_or(|s| r.scope == s));
    let (count, unread) = archived.fold((0, 0), |(count, unread), r| (count + 1, unread + u32::from(r.is_unread)));
    ArchiveSummary { count, unread }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::models::{NotificationMode, RoomPreview};

    pub(crate) fn room(id: &str, name: &str, direct: bool) -> RoomSummary {
        RoomSummary {
            id: id.into(),
            name: name.into(),
            avatar_url: None,
            avatar_seed: id.into(),
            dm_user_id: None,
            scope: scope_of(direct),
            is_direct: direct,
            is_dm: direct,
            is_encrypted: true,
            is_favourite: false,
            favourite_order: None,
            is_low_priority: false,
            is_archived: false,
            is_invite: false,
            is_public: false,
            inviter_name: None,
            topic: None,
            alias: None,
            member_count: 2,
            unread_count: 0,
            mention_count: 0,
            is_marked_unread: false,
            is_muted: false,
            notification_mode: NotificationMode::AllMessages,
            has_own_notification_mode: false,
            is_unread: false,
            unread_kind: UnreadKind::None,
            pinned_event_ids: vec![],
            preview: RoomPreview::Empty { is_encrypted: true },
            timestamp_ms: 0,
            draft: None,
            last_send_state: None,
        }
    }

    fn facts(unread: u64, mentions: u64, muted: bool) -> UnreadFacts {
        UnreadFacts {
            is_invite: false,
            is_marked_unread: false,
            mention_count: mentions,
            unread_count: unread,
            is_muted: muted,
        }
    }

    #[test]
    fn muted_rooms_are_quiet_except_for_mentions() {
        assert!(!is_unread(&facts(3, 0, true)));
        assert!(is_unread(&facts(3, 1, true)));
        assert_eq!(unread_kind(&facts(3, 1, true)), UnreadKind::Mention);
        assert_eq!(unread_kind(&facts(3, 0, false)), UnreadKind::Unread);
        assert_eq!(unread_kind(&facts(0, 0, false)), UnreadKind::None);
    }

    #[test]
    fn query_matches_word_starts_in_any_order() {
        let mut r = room("!a", "Zoë's Design Crit", false);
        r.alias = Some("#design:localhost".into());
        assert!(room_matches_query(&r, "crit zoe"));
        assert!(room_matches_query(&r, "desi"));
        assert!(room_matches_query(&r, "localhost"));
        assert!(!room_matches_query(&r, "marketing"));
        assert!(room_matches_query(&r, "  "));
    }

    #[test]
    fn lists_split_by_scope_with_invites_first() {
        let mut invite = room("!b", "Book Club", false);
        invite.is_invite = true;
        let rooms = vec![room("!a", "Design", false), invite, room("!c", "Maya", true)];
        let shown = filter_rooms(&rooms, Some(Scope::Rooms), ListFilter::All, "");
        assert_eq!(shown.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["!b", "!a"]);
        assert_eq!(filter_rooms(&rooms, Some(Scope::Chats), ListFilter::All, "").len(), 1);
        // The unified list keeps the list's order across both, invitations still first.
        let all = filter_rooms(&rooms, None, ListFilter::All, "");
        assert_eq!(all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["!b", "!a", "!c"]);
    }

    #[test]
    fn filters_and_badges() {
        let mut unread = room("!a", "Design", false);
        unread.is_unread = true;
        let mut fav = room("!b", "Book Club", false);
        fav.is_favourite = true;
        let rooms = vec![unread, fav];
        assert_eq!(filter_rooms(&rooms, Some(Scope::Rooms), ListFilter::Unread, "").len(), 1);
        assert_eq!(filter_rooms(&rooms, Some(Scope::Rooms), ListFilter::Favourites, "")[0].id, "!b");
        assert_eq!(filter_rooms(&rooms, None, ListFilter::Unread, "").len(), 1);
        assert_eq!(unread_total(&rooms, Some(Scope::Rooms)), 1);
        assert_eq!(unread_total(&rooms, Some(Scope::Chats)), 0);
        assert_eq!(unread_total(&rooms, None), 1);
    }

    #[test]
    fn archived_conversations_leave_the_list_and_its_badges() {
        let mut bot = room("!bot", "Telegram bridge bot", true);
        bot.is_archived = true;
        bot.is_unread = true;
        bot.is_favourite = true;
        let mut quiet = room("!old", "Old group", false);
        quiet.is_archived = true;
        let mut maya = room("!maya", "Maya", true);
        maya.is_unread = true;
        let rooms = vec![bot, quiet, maya];

        let ids = |filter, scope| -> Vec<String> {
            filter_rooms(&rooms, scope, filter, "").into_iter().map(|r| r.id).collect()
        };
        assert_eq!(ids(ListFilter::All, Some(Scope::Chats)), vec!["!maya"]);
        assert_eq!(ids(ListFilter::Unread, None), vec!["!maya"]);
        // Still pinned, so unarchiving puts it back where it was, but not among the pins now.
        assert!(ids(ListFilter::Favourites, None).is_empty());
        assert_eq!(ids(ListFilter::Archived, None), vec!["!bot", "!old"]);
        assert_eq!(ids(ListFilter::Archived, Some(Scope::Rooms)), vec!["!old"]);
        assert_eq!(filter_rooms(&rooms, None, ListFilter::Archived, "telegram").len(), 1);

        assert_eq!(unread_total(&rooms, Some(Scope::Chats)), 1);
        assert_eq!(archive_summary(&rooms, None), ArchiveSummary { count: 2, unread: 1 });
        assert_eq!(archive_summary(&rooms, Some(Scope::Rooms)), ArchiveSummary { count: 1, unread: 0 });
        assert_eq!(archive_summary(&rooms[2..], None), ArchiveSummary { count: 0, unread: 0 });
    }
}
