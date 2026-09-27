use super::*;

pub(super) fn model(host: &PopoverHost) -> ContextMenuModel {
    model_for_view(host.change_tracking_view)
}

fn model_for_view(view: ChangeTrackingView) -> ContextMenuModel {
    let check = |enabled: bool| enabled.then_some("icons/check.svg".into());

    ContextMenuModel::new(vec![
        ContextMenuItem::Header("Changes".into()),
        ContextMenuItem::Description("One list, or staged apart from the rest".into()),
        ContextMenuItem::Separator,
        ContextMenuItem::Entry {
            label: ChangeTrackingView::Unified.menu_label().into(),
            icon: check(view == ChangeTrackingView::Unified),
            shortcut: Some("U".into()),
            disabled: false,
            action: Box::new(ContextMenuAction::SetChangeTrackingView {
                view: ChangeTrackingView::Unified,
            }),
        },
        ContextMenuItem::Entry {
            label: ChangeTrackingView::Combined.menu_label().into(),
            icon: check(view == ChangeTrackingView::Combined),
            shortcut: Some("C".into()),
            disabled: false,
            action: Box::new(ContextMenuAction::SetChangeTrackingView {
                view: ChangeTrackingView::Combined,
            }),
        },
        ContextMenuItem::Entry {
            label: ChangeTrackingView::SplitUntracked.menu_label().into(),
            icon: check(view == ChangeTrackingView::SplitUntracked),
            shortcut: Some("S".into()),
            disabled: false,
            action: Box::new(ContextMenuAction::SetChangeTrackingView {
                view: ChangeTrackingView::SplitUntracked,
            }),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_marks_current_view() {
        let model = super::model_for_view(ChangeTrackingView::SplitUntracked);

        assert!(model.items.iter().any(|item| {
            matches!(
                item,
                ContextMenuItem::Entry { label, icon, .. }
                    if label.as_ref() == ChangeTrackingView::SplitUntracked.menu_label()
                        && icon
                            .as_ref()
                            .is_some_and(|icon| icon.as_ref() == "icons/check.svg")
            )
        }));
    }
}
