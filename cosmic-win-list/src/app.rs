// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    wayland_subscription::{
        OutputUpdate, ToplevelRequest, ToplevelUpdate, WaylandImage, WaylandRequest, WaylandUpdate,
        wayland_subscription,
    },
};
use cctk::{
    sctk::{output::OutputInfo, reexports::calloop::channel::Sender},
    toplevel_info::ToplevelInfo,
    wayland_client::protocol::{
        wl_output::WlOutput, wl_seat::WlSeat,
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    },
};
use cosmic::{
    Apply, Element, Task, app,
    applet::{
        Context, Size,
        cosmic_panel_config::{PanelAnchor, PanelSize},
    },
    cosmic_config::{Config, CosmicConfigEntry},
    desktop::IconSourceExt,
    iced::{
        self, Alignment, Background, Border, Length, Limits, Padding, Point, Subscription,
        advanced::text::{Ellipsize, EllipsizeHeightLimit},
        event::listen_with,
        platform_specific::shell::commands::popup::destroy_popup,
        runtime::{core::event, platform_specific::wayland::CornerRadius},
        widget::{
            Column, Row, column, mouse_area, row,
            rule::vertical as vertical_rule,
            space::{horizontal as horizontal_space, vertical as vertical_space},
            stack,
        },
        window,
    },
    surface::{self, action::LiveSettings},
    theme::{self, Button, Container},
    widget::{
        Image, button, container, divider,
        icon::{self, from_name},
        image::Handle,
        menu,
        rectangle_tracker::{RectangleTracker, RectangleUpdate, rectangle_tracker_subscription},
        svg, text,
    },
};
use cosmic::desktop::fde::{self, DesktopEntry, get_languages_from_env, unicase::Ascii};
use crate::config::{APP_ID, WinListConfig};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State;
use futures::future::pending;
use rustc_hash::FxHashMap;
use std::{borrow::Cow, rc::Rc, time::Duration};
use switcheroo_control::Gpu;
use tokio::time::sleep;

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<CosmicWinList>(())
}

#[derive(Debug, Clone)]
struct AppletIconData {
    icon_size: u16,
    icon_spacing: f32,
    padding: Padding,
}

impl AppletIconData {
    fn new(applet: &Context) -> Self {
        let icon_size = applet.suggested_size(false).0;
        let (major_padding, cross_padding) = applet.suggested_padding(false);
        let (h_padding, v_padding) = if applet.is_horizontal() {
            (major_padding as f32, cross_padding as f32)
        } else {
            (cross_padding as f32, major_padding as f32)
        };
        let icon_spacing = applet.spacing as f32;

        let padding = match applet.anchor {
            PanelAnchor::Top => [0.0, h_padding, v_padding, h_padding],
            PanelAnchor::Bottom => [v_padding, h_padding, 0.0, h_padding],
            PanelAnchor::Left => [v_padding, h_padding, v_padding, 0.0],
            PanelAnchor::Right => [v_padding, 0.0, v_padding, h_padding],
        };
        AppletIconData {
            icon_size,
            icon_spacing,
            padding: padding.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockItemId {
    Item(u32),
    ActiveOverflow,
    FavoritesOverflow,
}

impl From<u32> for DockItemId {
    fn from(id: u32) -> Self {
        DockItemId::Item(id)
    }
}

impl From<usize> for DockItemId {
    fn from(id: usize) -> Self {
        DockItemId::Item(id as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSource {
    Favorites,
    Active,
}

#[derive(Debug, Clone)]
struct DockItem {
    // ID used internally in the applet. Each dock item
    // have an unique id
    id: u32,
    toplevels: Vec<(ToplevelInfo, Option<WaylandImage>)>,
    // Information found in the .desktop file
    desktop_info: DesktopEntry,
    // We must use this because the id in `DesktopEntry` is an estimation.
    // Thus, if we unpin an item, we want to be sure to use the real id
    original_app_id: String,
}

impl DockItem {
    fn as_icon(
        &self,
        applet: &Context,
        rectangle_tracker: Option<&RectangleTracker<DockItemId>>,
        interaction_enabled: bool,
        gpus: Option<&[Gpu]>,
        is_focused: bool,
        dot_border_radius: [f32; 4],
        window_id: window::Id,
        filter: Option<&dyn Fn(&ToplevelInfo) -> bool>,
        drag_source: DragSource,
    ) -> Element<'_, Message> {
        let Self {
            toplevels,
            desktop_info,
            id,
            ..
        } = self;

        let filtered_toplevels: Vec<_> = if let Some(filter_fn) = filter {
            toplevels
                .iter()
                .filter(|(info, _)| filter_fn(info))
                .collect()
        } else {
            toplevels.iter().collect()
        };
        let toplevel_count = filtered_toplevels.len();

        let minimized = toplevel_count > 0
            && filtered_toplevels
                .iter()
                .all(|(info, _)| info.state.contains(&State::Minimized));

        let app_icon = AppletIconData::new(applet);

        let icon_scale: f32 = if minimized { 0.75 } else { 1.0 };
        let cosmic_icon = cosmic::widget::icon(
            fde::IconSource::from_unknown(desktop_info.icon().unwrap_or_default()).as_cosmic_icon(),
        )
        // sets the preferred icon size variant
        .size(128)
        .width(Length::Fixed(app_icon.icon_size as f32 * icon_scale))
        .height(Length::Fixed(app_icon.icon_size as f32 * icon_scale));

        let indicator = if is_focused {
            let line_length = app_icon.icon_size as f32 * 1.0;
            match applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => {
                    horizontal_space()
                        .width(Length::Fixed(4.0))
                        .height(line_length)
                }
                PanelAnchor::Top | PanelAnchor::Bottom => {
                    vertical_space()
                        .height(Length::Fixed(4.0))
                        .width(line_length)
                }
            }
            .apply(container)
            .class(theme::Container::custom(move |theme| container::Style {
                background: Some(Background::Color(theme.cosmic().accent_color().into())),
                border: Border {
                    radius: dot_border_radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            }))
        } else {
            match applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => {
                    horizontal_space().width(Length::Fixed(4.0))
                }
                PanelAnchor::Top | PanelAnchor::Bottom => {
                    vertical_space().height(Length::Fixed(4.0))
                }
            }
            .apply(container)
            .into()
        };

        let icon_wrapper: Element<_> = match applet.anchor {
            PanelAnchor::Left => row([
                indicator.into(),
                horizontal_space().width(Length::Fixed(1.0)).into(),
                cosmic_icon.clone().into(),
            ])
            .align_y(Alignment::Center)
            .into(),
            PanelAnchor::Right => row([
                cosmic_icon.clone().into(),
                horizontal_space().width(Length::Fixed(1.0)).into(),
                indicator.into(),
            ])
            .align_y(Alignment::Center)
            .into(),
            PanelAnchor::Top => column([
                indicator.into(),
                vertical_space().height(Length::Fixed(1.0)).into(),
                cosmic_icon.clone().into(),
            ])
            .align_x(Alignment::Center)
            .into(),
            PanelAnchor::Bottom => column([
                cosmic_icon.clone().into(),
                vertical_space().height(Length::Fixed(1.0)).into(),
                indicator.into(),
            ])
            .align_x(Alignment::Center)
            .into(),
        };

        let icon_button = button::custom(icon_wrapper)
            .padding(app_icon.padding)
            .selected(is_focused)
            .class(app_list_icon_style(is_focused));

        let click_command = if toplevel_count == 0 {
            launch_on_preferred_gpu(desktop_info, gpus)
        } else if toplevel_count == 1 {
            filtered_toplevels
                .first()
                .map(|t| Message::Toggle(t.0.foreign_toplevel.clone()))
        } else {
            Some(Message::ToplevelListPopup(*id, window_id))
        };

        let icon_button: Element<_> = if interaction_enabled {
            mouse_area(
                icon_button
                    .width(Length::Shrink)
                    .height(Length::Shrink),
            )
            .on_press(Message::DragPress(*id, drag_source, click_command.map(Box::new)))
            .on_release(Message::DragRelease(*id, drag_source))
            .on_drag(Message::DragStart(*id, drag_source))
            .on_right_release(Message::Popup(*id, window_id))
            .on_middle_release({
                launch_on_preferred_gpu(desktop_info, gpus)
                    .unwrap_or(Message::Popup(*id, window_id))
            })
            .into()
        } else {
            icon_button.into()
        };

        if let Some(tracker) = rectangle_tracker {
            tracker.container((*id).into(), icon_button).into()
        } else {
            icon_button.into()
        }
    }

    /// Tooltip text for this item. For ungrouped active windows we surface the
    /// individual toplevel's title; for pinned launchers (which carry no
    /// toplevels) we fall back to the desktop entry's localized full name.
    fn tooltip_text(&self, locales: &[String]) -> Cow<'_, str> {
        if let Some((info, _)) = self.toplevels.first() {
            if !info.title.is_empty() {
                return Cow::Borrowed(&info.title);
            }
        }
        self.desktop_info
            .full_name(locales)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct Popup {
    parent: window::Id,
    id: window::Id,
    dock_item: DockItem,
    popup_type: PopupType,
}

#[derive(Clone, Default)]
struct CosmicWinList {
    core: cosmic::app::Core,
    popup: Option<Popup>,
    subscription_ctr: u32,
    item_ctr: u32,
    desktop_entries: Vec<DesktopEntry>,
    active_list: Vec<DockItem>,
    pinned_list: Vec<DockItem>,
    config: WinListConfig,
    wayland_sender: Option<Sender<WaylandRequest>>,
    seat: Option<WlSeat>,
    rectangle_tracker: Option<RectangleTracker<DockItemId>>,
    rectangles: FxHashMap<DockItemId, iced::Rectangle>,
    gpus: Option<Vec<Gpu>>,
    active_workspaces: Vec<ExtWorkspaceHandleV1>,
    output_list: FxHashMap<WlOutput, OutputInfo>,
    locales: Vec<String>,
    hovered_toplevel: Option<ExtForeignToplevelHandleV1>,
    overflow_favorites_popup: Option<window::Id>,
    overflow_active_popup: Option<window::Id>,
    drag_state: Option<DragState>,
    drag_pending_command: Option<Box<Message>>,
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    item_id: u32,
    from: DragSource,
    last_cursor: Point,
    tick_scheduled: bool,
}

const DRAG_COOLDOWN_MS: u64 = 120;

#[derive(Debug, Clone, PartialEq)]
pub enum PopupType {
    RightClickMenu,
    ToplevelList,
}

#[derive(Debug, Clone)]
enum Message {
    Wayland(WaylandUpdate),
    PinApp(u32),
    UnpinApp(u32),
    Popup(u32, window::Id),
    Pressed(window::Id),
    ToplevelListPopup(u32, window::Id),
    ToplevelHoverChanged(ExtForeignToplevelHandleV1, bool),
    GpuRequest(Option<Vec<Gpu>>),
    CloseRequested(window::Id),
    ClosePopup,
    Activate(ExtForeignToplevelHandleV1),
    Toggle(ExtForeignToplevelHandleV1),
    Exec(String, Option<usize>, bool),
    CloseToplevel(ExtForeignToplevelHandleV1),
    Quit(u32),
    NewSeat(WlSeat),
    RemovedSeat,
    Rectangle(RectangleUpdate<DockItemId>),
    IncrementSubscriptionCtr,
    ConfigUpdated(WinListConfig),
    OpenFavorites,
    OpenActive,
    Surface(surface::Action),
    DragPress(u32, DragSource, Option<Box<Message>>),
    DragRelease(u32, DragSource),
    DragStart(u32, DragSource),
    DragMove(iced::Point),
    DragTick,
}

async fn try_get_gpus() -> Option<Vec<Gpu>> {
    let connection = zbus::Connection::system().await.ok()?;
    let proxy = switcheroo_control::SwitcherooControlProxy::new(&connection)
        .await
        .ok()?;

    if !proxy.has_dual_gpu().await.ok()? {
        return None;
    }

    let gpus = proxy.get_gpus().await.ok()?;
    if gpus.is_empty() {
        return None;
    }

    Some(gpus)
}

const TOPLEVEL_BUTTON_WIDTH: f32 = 192.0;
const TOPLEVEL_BUTTON_HEIGHT: f32 = 156.0;

fn toplevel_button<'a>(
    img: Option<WaylandImage>,
    title: String,
    handle: ExtForeignToplevelHandleV1,
    is_focused: bool,
    is_hovered: bool,
) -> Element<'a, Message> {
    let border = 1.0;
    let preview = column![
        container(if let Some(img) = img {
            Element::from(Image::new(Handle::from_rgba(
                img.width,
                img.height,
                img.img.clone(),
            )))
        } else {
            Image::new(Handle::from_rgba(1, 1, [0u8, 0u8, 0u8, 255u8].as_slice())).into()
        })
        .class(Container::custom(move |theme| container::Style {
            border: Border {
                color: theme.cosmic().bg_divider().into(),
                width: border,
                radius: 1.0.into(),
            },
            ..Default::default()
        }))
        .padding(border as u16)
        .apply(container)
        .center(Length::Fill),
        text::body(title)
            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
            .width(Length::Fill)
            .center()
    ]
    .spacing(4)
    .padding([4, 4, 0, 4]);
    let close_button_overlay = if is_hovered {
        row![
            horizontal_space(),
            button::custom(icon::from_name("window-close-symbolic").size(16))
                .class(Button::Destructive)
                .on_press(Message::CloseToplevel(handle.clone()))
                .padding(4)
        ]
    } else {
        row![]
    }
    .width(Length::Fill)
    .height(Length::Fill);

    stack![preview, close_button_overlay]
        .apply(button::custom)
        .on_press(Message::Toggle(handle.clone()))
        .class(window_menu_style(is_focused))
        .width(Length::Fixed(TOPLEVEL_BUTTON_WIDTH))
        .height(Length::Fixed(TOPLEVEL_BUTTON_HEIGHT))
        .padding(4)
        .selected(is_focused)
        .apply(mouse_area)
        .on_enter(Message::ToplevelHoverChanged(handle.clone(), true))
        .on_middle_press(Message::CloseToplevel(handle.clone()))
        .on_exit(Message::ToplevelHoverChanged(handle, false))
        .apply(Element::from)
}

fn window_menu_style(selected: bool) -> cosmic::theme::Button {
    let radius = theme::active()
        .cosmic()
        .radius_m()
        .map(|x| if x < 8.0 { x } else { x - 4.0 });

    Button::Custom {
        active: Box::new(move |focused, theme| {
            let a = button::Catalog::active(theme, focused, selected, &Button::AppletMenu);
            button::Style {
                background: if selected {
                    Some(Background::Color(
                        theme.cosmic().icon_button.selected_state_color().into(),
                    ))
                } else {
                    a.background
                },
                border_radius: radius.into(),
                outline_width: 0.0,
                ..a
            }
        }),
        hovered: Box::new(move |focused, theme| {
            let focused = selected || focused;
            let text = button::Catalog::hovered(theme, focused, focused, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
        disabled: Box::new(move |theme| {
            let text = button::Catalog::disabled(theme, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
        pressed: Box::new(move |focused, theme| {
            let focused = selected || focused;
            let text = button::Catalog::pressed(theme, focused, focused, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
    }
}

fn app_list_icon_style(selected: bool) -> cosmic::theme::Button {
    Button::Custom {
        active: Box::new(move |focused, theme| {
            let a = button::Catalog::active(theme, focused, selected, &Button::AppletIcon);
            button::Style {
                background: if selected {
                    Some(Background::Color(
                        theme.cosmic().icon_button.selected_state_color().into(),
                    ))
                } else {
                    a.background
                },
                ..a
            }
        }),
        hovered: Box::new(move |focused, theme| {
            button::Catalog::hovered(theme, focused, selected, &Button::AppletIcon)
        }),
        disabled: Box::new(|theme| button::Catalog::disabled(theme, &Button::AppletIcon)),
        pressed: Box::new(move |focused, theme| {
            button::Catalog::pressed(theme, focused, selected, &Button::AppletIcon)
        }),
    }
}

#[inline]
pub fn menu_control_padding() -> Padding {
    let spacing = theme::spacing();
    [spacing.space_xxs, spacing.space_s].into()
}

fn find_desktop_entries<'a>(
    desktop_entries: &'a [fde::DesktopEntry],
    app_ids: &'a [String],
) -> impl Iterator<Item = fde::DesktopEntry> + 'a {
    app_ids.iter().map(|fav| {
        let unicase_fav = fde::unicase::Ascii::new(fav.as_str());
        fde::find_app_by_id(desktop_entries, unicase_fav).map_or_else(
            || fde::DesktopEntry::from_appid(fav.clone()),
            ToOwned::to_owned,
        )
    })
}

impl CosmicWinList {
    // Cache all desktop entries to use when new apps are added to the dock.
    fn update_desktop_entries(&mut self) {
        self.desktop_entries = fde::Iter::new(fde::default_paths())
            .filter_map(|p| fde::DesktopEntry::from_path(p, Some(&self.locales)).ok())
            .collect::<Vec<_>>();
    }

    fn is_on_current_monitor_and_workspace(&self, toplevel_info: &ToplevelInfo) -> bool {
        use crate::config::ToplevelFilter;

        let on_active_workspace = self.active_workspaces.is_empty()
            || toplevel_info.workspace.is_empty()
            || self
                .active_workspaces
                .iter()
                .any(|workspace| toplevel_info.workspace.contains(workspace));

        let on_active_output = if toplevel_info.output.is_empty() {
            true
        } else {
            self.output_list
                .iter()
                .find(|(_, info)| info.name.as_ref() == Some(&self.core.applet.output_name))
                .map_or(true, |(active_output, _)| {
                    toplevel_info
                        .output
                        .iter()
                        .any(|output| output == active_output)
                })
        };

        match &self.config.filter_top_levels {
            None => on_active_output,
            Some(ToplevelFilter::ActiveWorkspace) => on_active_workspace,
            Some(ToplevelFilter::ConfiguredOutput) => on_active_output && on_active_workspace,
        }
    }

    // Update pinned items using the cached desktop entries as a source.
    fn update_pinned_list(&mut self) {
        self.pinned_list = find_desktop_entries(&self.desktop_entries, &self.config.favorites)
            .zip(&self.config.favorites)
            .enumerate()
            .map(|(pinned_ctr, (e, original_id))| DockItem {
                id: pinned_ctr as u32,
                toplevels: Vec::new(),
                desktop_info: e,
                original_app_id: original_id.clone(),
            })
            .collect();
    }

    /// Close any open popups.
    fn close_popups(&mut self) -> Task<cosmic::Action<Message>> {
        let mut commands = Vec::new();
        if let Some(popup) = self.popup.take() {
            commands.push(destroy_popup(popup.id));
        }
        if let Some(popup) = self.overflow_active_popup.take() {
            commands.push(destroy_popup(popup));
        }
        if let Some(popup) = self.overflow_favorites_popup.take() {
            commands.push(destroy_popup(popup));
        }
        Task::batch(commands)
    }

    /// Returns the length of the group in the favorite list after which items are displayed in a popup.
    /// Shrink the favorite list until it only has active windows, or until it fits in the length provided.
    fn panel_overflow_lengths(&self) -> (Option<usize>, Option<usize>) {
        let mut favorite_index;
        let mut active_index = None;
        let Some(mut max_major_axis_len) = self.core.applet.suggested_bounds.as_ref().map(|c| {
            // if we have a configure for width and height, we're in a overflow popup
            match self.core.applet.anchor {
                PanelAnchor::Top | PanelAnchor::Bottom => c.width as u32,
                PanelAnchor::Left | PanelAnchor::Right => c.height as u32,
            }
        }) else {
            return (None, active_index);
        };
        // subtract the divider width
        max_major_axis_len -= 1;
        let applet_icon = AppletIconData::new(&self.core.applet);

        let button_total_size = self.core.applet.suggested_size(true).0
            + self.core.applet.suggested_padding(true).0 * 2
            + applet_icon.icon_spacing as u16;

        // Only pinned launchers whose app has *no* open windows are visible – any
        // pinned item whose app is running is hidden so that the same app icon
        // never appears twice in ungrouped mode.
        let visible_pinned_len = self
            .pinned_list
            .iter()
            .filter(|item| !self.pinned_has_active_window(&item.original_app_id))
            .count();

        // initial calculation of favorite_index
        let btn_count = max_major_axis_len / button_total_size as u32;
        if btn_count >= visible_pinned_len as u32 + self.active_list.len() as u32 {
            return (None, active_index);
        } else {
            favorite_index = (btn_count as usize).min(visible_pinned_len).max(2);
        }

        // calculation of active_index based on favorite_index if there is still not enough space
        let active_index_max = (btn_count as i32)
            - (visible_pinned_len as i32).saturating_sub(favorite_index as i32);
        if active_index_max >= self.active_list.len() as i32 {
            active_index = Some(self.active_list.len());
        } else {
            active_index = Some((active_index_max.max(2) as usize).min(self.active_list.len()));
        }

        // final calculation of favorite_index if there is still not enough space
        if let Some(active_index) = active_index {
            let favorite_index_max = (btn_count as i32) - active_index as i32;
            favorite_index = favorite_index_max.max(2) as usize;
        } else {
            favorite_index = (btn_count as usize).min(visible_pinned_len);
        }
        (Some(favorite_index), active_index)
    }

    fn currently_active_toplevel(&self) -> Vec<ExtForeignToplevelHandleV1> {
        if self.active_workspaces.is_empty() {
            return Vec::new();
        }
        let current_output = &self.core.applet.output_name;
        let mut focused_toplevels: Vec<ExtForeignToplevelHandleV1> = Vec::new();
        let active_workspaces = &self.active_workspaces;
        for toplevel_list in self.active_list.iter().chain(self.pinned_list.iter()) {
            for (t_info, _) in &toplevel_list.toplevels {
                if t_info.state.contains(&State::Activated)
                    && active_workspaces
                        .iter()
                        .any(|workspace| t_info.workspace.contains(workspace))
                    && (t_info.output.is_empty()
                        || t_info.output.iter().any(|x| {
                            self.output_list.get(x).is_some_and(|val| {
                                val.name.as_ref().is_some_and(|n| n == current_output)
                            })
                        }))
                {
                    focused_toplevels.push(t_info.foreign_toplevel.clone());
                }
            }
        }
        focused_toplevels
    }

    fn find_desktop_entry_for_toplevel(
        &mut self,
        info: &ToplevelInfo,
        unicase_appid: Ascii<&str>,
    ) -> DesktopEntry {
        if let Some(appid) = fde::find_app_by_id(&self.desktop_entries, unicase_appid) {
            appid.clone()
        } else {
            // Update desktop entries in case it was not found.
            self.update_desktop_entries();
            if let Some(appid) = fde::find_app_by_id(&self.desktop_entries, unicase_appid) {
                appid.clone()
            } else {
                tracing::error!(id = info.app_id, "could not find desktop entry for app");
                let mut fallback_entry = fde::DesktopEntry::from_appid(info.app_id.clone());
                // proton opens games as steam_app_X, where X is either
                // the steam appid or "default". games with a steam appid
                // can have a desktop entry generated elsewhere; this
                // specifically handles non-steam games opened
                // under proton
                // in addition, try to match WINE entries who have its
                // appid = the full name of the executable (incl. .exe)
                let is_proton_game = info.app_id == "steam_app_default";
                if is_proton_game || info.app_id.ends_with(".exe") {
                    for entry in &self.desktop_entries {
                        let localised_name = entry.name(&self.locales).unwrap_or_default();
                        if localised_name == info.title {
                            // if this is a proton game, we only want
                            // to look for game entries
                            if is_proton_game
                                && !entry.categories().unwrap_or_default().contains(&"Game")
                            {
                                continue;
                            }
                            fallback_entry = entry.clone();
                            break;
                        }
                    }
                }
                fallback_entry
            }
        }
    }

    // Check if a specific toplevel is focused
    fn is_focused(&self, handle: &ExtForeignToplevelHandleV1) -> bool {
        self.currently_active_toplevel().contains(handle)
    }

    // Check if a specific toplevel button is currently hovered
    fn is_hovered(&self, handle: &ExtForeignToplevelHandleV1) -> bool {
        self.hovered_toplevel.as_ref() == Some(handle)
    }

    /// A pinned launcher is hidden when at least one window of the same app is
    /// open (windows are never grouped, so showing the launcher next to the
    /// windows would duplicate the icon).
    fn pinned_has_active_window(&self, pinned_app_id: &str) -> bool {
        self.active_list
            .iter()
            .any(|item| {
                item.original_app_id == pinned_app_id
                    && item
                        .toplevels
                        .iter()
                        .any(|(info, _)| self.is_on_current_monitor_and_workspace(info))
            })
    }
}

impl cosmic::Application for CosmicWinList {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = APP_ID;

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let config = Config::new(APP_ID, WinListConfig::VERSION)
            .ok()
            .and_then(|c| WinListConfig::get_entry(&c).ok())
            .unwrap_or_default();

        let mut app_list = Self {
            core,
            config,
            locales: get_languages_from_env(),
            ..Default::default()
        };

        app_list.update_desktop_entries();
        app_list.update_pinned_list();

        app_list.item_ctr = app_list.pinned_list.len() as u32;

        (
            app_list,
            Task::perform(try_get_gpus(), |gpus| {
                cosmic::Action::App(Message::GpuRequest(gpus))
            }),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Popup(id, parent_window_id) => {
                if let Some(Popup {
                    parent,
                    id: popup_id,
                    ..
                }) = self.popup.take()
                {
                    if parent == parent_window_id {
                        return destroy_popup(popup_id);
                    } else {
                        self.overflow_active_popup = None;
                        self.overflow_favorites_popup = None;
                        return Task::batch([destroy_popup(popup_id), destroy_popup(parent)]);
                    }
                }
                if let Some(toplevel_group) = self
                    .active_list
                    .iter()
                    .chain(self.pinned_list.iter())
                    .find(|t| t.id == id)
                    .cloned()
                {
                    let Some(rectangle) = self.rectangles.get(&toplevel_group.id.into()).copied()
                    else {
                        tracing::error!("No rectangle found for toplevel group");
                        return Task::none();
                    };
                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            move |_| LiveSettings {
                                corners: Some(CornerRadius::default()),
                                ..Default::default()
                            },
                            move |app: &mut Self| {
                                let new_id = window::Id::unique();
                                app.popup = Some(Popup {
                                    parent: parent_window_id,
                                    id: new_id,
                                    dock_item: toplevel_group.clone(),
                                    popup_type: PopupType::RightClickMenu,
                                });

                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    parent_window_id,
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );
                                let iced::Rectangle {
                                    x,
                                    y,
                                    width,
                                    height,
                                } = rectangle;
                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };
                                popup_settings
                            },
                            None,
                        ));

                    let gpu_update = Task::perform(try_get_gpus(), |gpus| {
                        cosmic::Action::App(Message::GpuRequest(gpus))
                    });
                    return Task::batch([gpu_update, popup_task]);
                }
            }
            Message::ToplevelListPopup(id, parent_window_id) => {
                if let Some(Popup {
                    parent,
                    id: popup_id,
                    ..
                }) = self.popup.take()
                {
                    if parent == parent_window_id {
                        return destroy_popup(popup_id);
                    } else {
                        self.overflow_active_popup = None;
                        self.overflow_favorites_popup = None;
                        return Task::batch([destroy_popup(popup_id), destroy_popup(parent)]);
                    }
                }
                if let Some(toplevel_group) = self
                    .active_list
                    .iter()
                    .chain(self.pinned_list.iter())
                    .find(|t| t.id == id)
                {
                    for (info, _) in &toplevel_group.toplevels {
                        if let Some(tx) = self.wayland_sender.as_ref() {
                            let _ =
                                tx.send(WaylandRequest::Screencopy(info.foreign_toplevel.clone()));
                        }
                    }

                    let Some(rectangle) = self.rectangles.get(&toplevel_group.id.into()).copied()
                    else {
                        return Task::none();
                    };
                    let new_id = window::Id::unique();
                    self.popup = Some(Popup {
                        parent: parent_window_id,
                        id: new_id,
                        dock_item: toplevel_group.clone(),
                        popup_type: PopupType::ToplevelList,
                    });
                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| LiveSettings {
                                corners: Some(CornerRadius::default()),
                                ..Default::default()
                            },
                            move |app: &mut Self| {
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    parent_window_id,
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );
                                let iced::Rectangle {
                                    x,
                                    y,
                                    width,
                                    height,
                                } = rectangle;
                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };
                                let max_windows = 7.0;
                                let window_spacing = 8.0;
                                popup_settings.positioner.size_limits = match app.core.applet.anchor
                                {
                                    PanelAnchor::Right | PanelAnchor::Left => Limits::NONE
                                        .min_width(100.0)
                                        .min_height(30.0)
                                        .max_width(window_spacing * 2.0 + TOPLEVEL_BUTTON_WIDTH)
                                        .max_height(
                                            TOPLEVEL_BUTTON_HEIGHT * max_windows
                                                + window_spacing * (max_windows + 1.0),
                                        ),
                                    PanelAnchor::Bottom | PanelAnchor::Top => Limits::NONE
                                        .min_width(30.0)
                                        .min_height(100.0)
                                        .max_width(
                                            TOPLEVEL_BUTTON_WIDTH * max_windows
                                                + window_spacing * (max_windows + 1.0),
                                        )
                                        .max_height(window_spacing * 2.0 + TOPLEVEL_BUTTON_HEIGHT),
                                };
                                popup_settings
                            },
                            None,
                        ));

                    return popup_task;
                }
            }
            Message::ToplevelHoverChanged(handle, entering) => {
                match (entering, &self.hovered_toplevel) {
                    (true, _) => self.hovered_toplevel = Some(handle),
                    // prevents race condition
                    (false, Some(h)) if h == &handle => self.hovered_toplevel = None,
                    _ => {}
                }
            }
            Message::PinApp(id) => {
                // Pin the application associated with this window as a separate
                // launcher. The window itself stays in the active list so that
                // each of its siblings remains individually accessible.
                if let Some(entry) = self.active_list.iter().find(|t| t.id == id).cloned() {
                    let id_str = if entry.original_app_id.is_empty() {
                        entry.desktop_info.id().to_string()
                    } else {
                        entry.original_app_id.clone()
                    };
                    self.config.add_pinned(
                        id_str,
                        &Config::new(APP_ID, WinListConfig::VERSION).unwrap(),
                    );

                    // Only add a new pinned launcher if one for this app_id does
                    // not already exist.
                    let already_pinned = self
                        .pinned_list
                        .iter()
                        .any(|p| p.original_app_id == entry.original_app_id);
                    if !already_pinned {
                        self.item_ctr += 1;
                        self.pinned_list.push(DockItem {
                            id: self.item_ctr,
                            toplevels: Vec::new(),
                            desktop_info: entry.desktop_info.clone(),
                            original_app_id: entry.original_app_id.clone(),
                        });
                    }
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::UnpinApp(id) => {
                if let Some(i) = self.pinned_list.iter().position(|t| t.id == id) {
                    let entry = self.pinned_list.remove(i);

                    self.config.remove_pinned(
                        &entry.original_app_id,
                        &Config::new(APP_ID, WinListConfig::VERSION).unwrap(),
                    );

                    self.rectangles.remove(&entry.id.into());
                    // Pinned items never hold toplevels, so there is nothing to
                    // promote back to the active list.
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::Activate(handle) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::Toplevel(ToplevelRequest::Activate(handle)));
                }
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p.id);
                }
            }
            Message::Toggle(handle) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::Toplevel(if self.is_focused(&handle) {
                        ToplevelRequest::Minimize(handle)
                    } else {
                        ToplevelRequest::Activate(handle)
                    }));
                }
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p.id);
                }
            }
            Message::CloseToplevel(handle) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::Toplevel(ToplevelRequest::Quit(handle)));
                }
            }
            Message::Quit(id) => {
                // Quit closes the single toplevel owned by the dock item whose id
                // was captured in the right-click popup.
                if let Some(toplevel_group) = self
                    .active_list
                    .iter()
                    .chain(self.pinned_list.iter())
                    .find(|t| t.id == id)
                {
                    for (info, _) in &toplevel_group.toplevels {
                        if let Some(tx) = self.wayland_sender.as_ref() {
                            let _ = tx.send(WaylandRequest::Toplevel(ToplevelRequest::Quit(
                                info.foreign_toplevel.clone(),
                            )));
                        }
                    }
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::Wayland(event) => {
                match event {
                    WaylandUpdate::Init(tx) => {
                        self.wayland_sender.replace(tx);
                    }
                    WaylandUpdate::Image(handle, img) => {
                        'img_update: for x in self
                            .active_list
                            .iter_mut()
                            .chain(self.pinned_list.iter_mut())
                        {
                            if let Some((_, handle_img)) = x
                                .toplevels
                                .iter_mut()
                                .find(|(info, _)| info.foreign_toplevel == handle)
                            {
                                *handle_img = Some(img);
                                break 'img_update;
                            }
                        }
                    }
                    WaylandUpdate::Finished => {
                        for t in &mut self.pinned_list {
                            t.toplevels.clear();
                        }
                        self.active_list.clear();
                        let subscription_ctr = self.subscription_ctr;
                        let rand_d = fastrand::u64(0..100);
                        return iced::Task::perform(
                            async move {
                                if let Some(millis) = 2u64
                                    .checked_pow(subscription_ctr)
                                    .and_then(|d| d.checked_add(rand_d))
                                {
                                    sleep(Duration::from_millis(millis)).await;
                                } else {
                                    pending::<()>().await;
                                }
                            },
                            |()| Message::IncrementSubscriptionCtr,
                        )
                        .map(cosmic::action::app);
                    }
                    WaylandUpdate::Toplevel(event) => match event {
                        // Each window is shown as its own dock item; windows of the same
                        // application are never grouped together.
                        ToplevelUpdate::Add(mut info) => {
                            let unicase_appid = fde::unicase::Ascii::new(&*info.app_id);
                            let new_desktop_info =
                                self.find_desktop_entry_for_toplevel(&info, unicase_appid);

                            if info.app_id.is_empty() {
                                info.app_id = format!("Unknown Application {}", self.item_ctr);
                            }
                            self.item_ctr += 1;

                            self.active_list.push(DockItem {
                                id: self.item_ctr,
                                original_app_id: info.app_id.clone(),
                                toplevels: vec![(info, None)],
                                desktop_info: new_desktop_info,
                            });
                        }
                        ToplevelUpdate::Remove(handle) => {
                            self.active_list
                                .retain(|t| !t.toplevels.iter().any(|(info, _)| info.foreign_toplevel == handle));

                            if let Some(popup) = &mut self.popup
                                && popup.popup_type == PopupType::ToplevelList
                            {
                                popup
                                    .dock_item
                                    .toplevels
                                    .retain(|(info, _)| info.foreign_toplevel != handle);

                                if popup.dock_item.toplevels.is_empty() {
                                    let id = popup.id;
                                    self.popup = None;
                                    return destroy_popup(id);
                                }
                            }
                        }
                        ToplevelUpdate::Update(info) => {
                            if info.app_id.is_empty() {
                                return Task::none();
                            }

                            // Locate the dock item (active list only; pinned items
                            // never hold toplevels) that owns this toplevel.
                            let owner_idx = self
                                .active_list
                                .iter()
                                .position(|item| {
                                    item.toplevels
                                        .iter()
                                        .any(|(t_info, _)| t_info.foreign_toplevel == info.foreign_toplevel)
                                });

                            if let Some(idx) = owner_idx {
                                let item = &mut self.active_list[idx];
                                let toplevel = item
                                    .toplevels
                                    .iter_mut()
                                    .find(|(t_info, _)| t_info.foreign_toplevel == info.foreign_toplevel);
                                let updated_appid = if let Some((t_info, _)) = toplevel {
                                    let changed = info.app_id != t_info.app_id;
                                    let saved_output = t_info.output.clone();
                                    *t_info = info.clone();
                                    if t_info.output.is_empty() && !saved_output.is_empty() {
                                        t_info.output = saved_output;
                                    }
                                    changed
                                } else {
                                    false
                                };

                                // refresh desktop info only if app_id changed; do this
                                // outside the per-item mutable borrow by indexing
                                if updated_appid {
                                    let new_desktop_entry = self.find_desktop_entry_for_toplevel(
                                        &info,
                                        Ascii::new(&info.app_id),
                                    );
                                    let item = &mut self.active_list[idx];
                                    item.desktop_info = new_desktop_entry;
                                    item.original_app_id = info.app_id.clone();
                                }
                            }
                        }
                    },
                    WaylandUpdate::Workspace(workspaces) => self.active_workspaces = workspaces,
                    WaylandUpdate::Output(event) => match event {
                        OutputUpdate::Add(output, info) => {
                            self.output_list.insert(output, info);
                        }
                        OutputUpdate::Update(output, info) => {
                            self.output_list.insert(output, info);
                        }
                        OutputUpdate::Remove(output) => {
                            self.output_list.remove(&output);
                        }
                    },
                    WaylandUpdate::ActivationToken {
                        token,
                        app_id,
                        exec,
                        gpu_idx,
                        terminal,
                    } => {
                        let mut envs = Vec::new();
                        if let Some(token) = token {
                            envs.push(("XDG_ACTIVATION_TOKEN".to_string(), token.clone()));
                            envs.push(("DESKTOP_STARTUP_ID".to_string(), token));
                        }
                        if let (Some(gpus), Some(idx)) = (self.gpus.as_ref(), gpu_idx) {
                            envs.extend(
                                gpus[idx]
                                    .environment
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                        }
                        tokio::spawn(async move {
                            cosmic::desktop::spawn_desktop_exec(
                                exec,
                                envs,
                                app_id.as_deref(),
                                terminal,
                            )
                            .await;
                        });
                    }
                }
            }
            Message::NewSeat(s) => {
                self.seat.replace(s);
            }
            Message::RemovedSeat => {
                self.seat.take();
            }
            Message::Exec(exec, gpu_idx, terminal) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec,
                        gpu_idx,
                        terminal,
                    });
                }
            }
            Message::Rectangle(u) => match u {
                RectangleUpdate::Rectangle(r) => {
                    self.rectangles.insert(r.0, r.1);
                }
                RectangleUpdate::Init(tracker) => {
                    self.rectangle_tracker.replace(tracker);
                }
            },
            Message::ClosePopup => {
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p.id);
                }
            }
            Message::IncrementSubscriptionCtr => {
                self.subscription_ctr += 1;
            }
            Message::ConfigUpdated(config) => {
                self.config = config;
                // Pinned launchers never carry toplevels in ungrouped mode, so we
                // simply rebuild them from the configured favorites without
                // touching the active list.
                self.pinned_list =
                    find_desktop_entries(&self.desktop_entries, &self.config.favorites)
                        .zip(&self.config.favorites)
                        .enumerate()
                        .map(|(pinned_ctr, (de, original_id))| DockItem {
                            id: pinned_ctr as u32,
                            toplevels: Vec::new(),
                            desktop_info: de,
                            original_app_id: original_id.clone(),
                        })
                        .collect();

                self.item_ctr = self.item_ctr.max(self.pinned_list.len() as u32);
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup.as_ref().map(|p| p.id) {
                    self.popup = None;
                }
                if self.overflow_active_popup.is_some_and(|p| p == id) {
                    self.overflow_active_popup = None;
                }
                if self.overflow_favorites_popup.is_some_and(|p| p == id) {
                    self.overflow_favorites_popup = None;
                }
            }
            Message::GpuRequest(gpus) => {
                self.gpus = gpus;
            }
            Message::OpenActive => {
                let create_new = self.overflow_active_popup.is_none();
                let mut cmds = vec![self.close_popups()];

                // create a popup with the active list
                if create_new {
                    let new_id = window::Id::unique();
                    self.overflow_active_popup = Some(new_id);
                    let Some(iced::Rectangle {
                        x,
                        y,
                        width,
                        height,
                    }) = self.rectangles.get(&DockItemId::ActiveOverflow).copied()
                    else {
                        return Task::none();
                    };

                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| LiveSettings {
                                corners: Some(CornerRadius::default()),
                                ..Default::default()
                            },
                            move |app: &mut Self| {
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    app.core.main_window_id().unwrap(),
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );

                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };

                                let applet_suggested_size = app.core.applet.suggested_size(false).0
                                    + 2 * app.core.applet.suggested_padding(false).0;
                                let (_favorite_popup_cutoff, active_popup_cutoff) =
                                    app.panel_overflow_lengths();
                                let popup_applet_count = app.active_list.len().saturating_sub(
                                    (active_popup_cutoff.unwrap_or_default()).saturating_sub(1),
                                ) as f32;
                                let popup_applet_size = applet_suggested_size as f32
                                    * popup_applet_count
                                    + 4.0 * (popup_applet_count - 1.);
                                let (max_width, max_height) = match app.core.applet.anchor {
                                    PanelAnchor::Top | PanelAnchor::Bottom => {
                                        (popup_applet_size, applet_suggested_size as f32)
                                    }
                                    PanelAnchor::Left | PanelAnchor::Right => {
                                        (applet_suggested_size as f32, popup_applet_size)
                                    }
                                };
                                popup_settings.positioner.size_limits = Limits::NONE
                                    .max_width(max_width)
                                    .min_width(1.)
                                    .max_height(max_height)
                                    .min_height(1.);
                                popup_settings
                            },
                            None,
                        ));
                    cmds.push(popup_task);
                }
                return Task::batch(cmds);
            }
            Message::OpenFavorites => {
                let create_new = self.overflow_favorites_popup.is_none();
                let mut cmds = vec![self.close_popups()];

                // create a popup with the favorites list
                if create_new {
                    let new_id = window::Id::unique();
                    self.overflow_favorites_popup = Some(new_id);
                    let Some(iced::Rectangle {
                        x,
                        y,
                        width,
                        height,
                    }) = self.rectangles.get(&DockItemId::FavoritesOverflow).copied()
                    else {
                        return Task::none();
                    };

                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| LiveSettings {
                                corners: Some(CornerRadius::default()),
                                ..Default::default()
                            },
                            move |app: &mut Self| {
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    app.core.main_window_id().unwrap(),
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );

                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };

                                let applet_suggested_size = app.core.applet.suggested_size(false).0
                                    + 2 * app.core.applet.suggested_padding(false).0;
                                let (favorite_popup_cutoff, _active_popup_cutoff) =
                                    app.panel_overflow_lengths();
                                let popup_applet_count = app.pinned_list.len().saturating_sub(
                                    favorite_popup_cutoff.unwrap_or_default().saturating_sub(1),
                                ) as f32;
                                let popup_applet_size = applet_suggested_size as f32
                                    * popup_applet_count
                                    + 4.0 * (popup_applet_count - 1.);
                                let (max_width, max_height) = match app.core.applet.anchor {
                                    PanelAnchor::Top | PanelAnchor::Bottom => {
                                        (popup_applet_size, applet_suggested_size as f32)
                                    }
                                    PanelAnchor::Left | PanelAnchor::Right => {
                                        (applet_suggested_size as f32, popup_applet_size)
                                    }
                                };
                                popup_settings.positioner.size_limits = Limits::NONE
                                    .max_width(max_width)
                                    .min_width(1.)
                                    .max_height(max_height)
                                    .min_height(1.);
                                popup_settings
                            },
                            None,
                        ));
                    cmds.push(popup_task);
                }
                return Task::batch(cmds);
            }
            Message::Pressed(id) => {
                if self.popup.is_some() && self.core.main_window_id() == Some(id) {
                    return self.close_popups();
                }
            }
            Message::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(a),
                ));
            }
            Message::DragPress(_id, _source, command) => {
                self.drag_pending_command = command;
            }
            Message::DragRelease(_id, source) => {
                if self.drag_state.is_some() {
                    self.drag_state = None;
                    self.drag_pending_command = None;
                    if source == DragSource::Favorites {
                        let new_order: Vec<String> = self
                            .pinned_list
                            .iter()
                            .map(|item| item.original_app_id.clone())
                            .collect();
                        if let Ok(config) = Config::new(APP_ID, WinListConfig::VERSION) {
                            self.config.update_pinned(new_order, &config);
                        }
                    }
                } else if let Some(cmd) = self.drag_pending_command.take() {
                    return Task::done(cosmic::Action::App(*cmd));
                }
                self.drag_pending_command = None;
            }
            Message::DragStart(id, source) => {
                self.drag_pending_command = None;
                self.drag_state = Some(DragState {
                    item_id: id,
                    from: source,
                    last_cursor: Point::new(0.0, 0.0),
                    tick_scheduled: false,
                });
            }
            Message::DragMove(position) => {
                if let Some(ref mut drag) = self.drag_state {
                    drag.last_cursor = position;
                    if !drag.tick_scheduled {
                        drag.tick_scheduled = true;
                        return Task::perform(
                            tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                            |_| cosmic::Action::App(Message::DragTick),
                        );
                    }
                }
            }
            Message::DragTick => {
                let Some(drag) = self.drag_state else {
                    return Task::none();
                };
                let is_horizontal = match self.core.applet.anchor {
                    PanelAnchor::Top | PanelAnchor::Bottom => true,
                    PanelAnchor::Left | PanelAnchor::Right => false,
                };

                let drag_rect = self.rectangles.get(&DockItemId::Item(drag.item_id)).copied();
                let Some(drag_rect) = drag_rect else {
                    return Task::perform(
                        tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                        |_| cosmic::Action::App(Message::DragTick),
                    );
                };

                let drag_center = if is_horizontal {
                    drag_rect.x + drag_rect.width / 2.0
                } else {
                    drag_rect.y + drag_rect.height / 2.0
                };

                let cursor_val = if is_horizontal {
                    drag.last_cursor.x
                } else {
                    drag.last_cursor.y
                };

                let direction: i32 = if cursor_val > drag_center { 1 } else { -1 };

                let list: &Vec<DockItem> = match drag.from {
                    DragSource::Favorites => &self.pinned_list,
                    DragSource::Active => &self.active_list,
                };

                let Some(drag_idx) = list.iter().position(|item| item.id == drag.item_id) else {
                    self.drag_state = None;
                    return Task::none();
                };

                let visible_items: Vec<(usize, u32)> = list
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        self.rectangles
                            .contains_key(&DockItemId::Item(item.id))
                    })
                    .map(|(i, item)| (i, item.id))
                    .collect();

                let Some(visible_pos) = visible_items
                    .iter()
                    .position(|(_, id)| *id == drag.item_id)
                else {
                    return Task::perform(
                        tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                        |_| cosmic::Action::App(Message::DragTick),
                    );
                };

                let target_visible_pos = if direction > 0 {
                    visible_pos + 1
                } else {
                    match visible_pos.checked_sub(1) {
                        Some(p) => p,
                        None => {
                            return Task::perform(
                                tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                                |_| cosmic::Action::App(Message::DragTick),
                            );
                        }
                    }
                };

                if target_visible_pos >= visible_items.len() {
                    return Task::perform(
                        tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                        |_| cosmic::Action::App(Message::DragTick),
                    );
                }

                let (target_list_idx, target_id) = visible_items[target_visible_pos];
                let target_rect = self
                    .rectangles
                    .get(&DockItemId::Item(target_id))
                    .copied();

                let Some(target_rect) = target_rect else {
                    return Task::perform(
                        tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                        |_| cosmic::Action::App(Message::DragTick),
                    );
                };

                let dragged_edge = if direction > 0 {
                    if is_horizontal {
                        drag_rect.x + drag_rect.width
                    } else {
                        drag_rect.y + drag_rect.height
                    }
                } else if is_horizontal {
                    drag_rect.x
                } else {
                    drag_rect.y
                };
                let target_edge = if direction > 0 {
                    if is_horizontal {
                        target_rect.x
                    } else {
                        target_rect.y
                    }
                } else if is_horizontal {
                    target_rect.x + target_rect.width
                } else {
                    target_rect.y + target_rect.height
                };

                let crossed = if direction > 0 {
                    cursor_val > (dragged_edge + target_edge) / 2.0
                } else {
                    cursor_val < (dragged_edge + target_edge) / 2.0
                };

                if crossed {
                    let list: &mut Vec<DockItem> = match drag.from {
                        DragSource::Favorites => &mut self.pinned_list,
                        DragSource::Active => &mut self.active_list,
                    };
                    list.swap(drag_idx, target_list_idx);
                }

                return Task::perform(
                    tokio::time::sleep(Duration::from_millis(DRAG_COOLDOWN_MS)),
                    |_| cosmic::Action::App(Message::DragTick),
                );
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let focused_item = self.currently_active_toplevel();
        let theme = self.core.system_theme();
        let dot_radius = theme.cosmic().radius_xs();
        let app_icon = AppletIconData::new(&self.core.applet);
        let is_horizontal = match self.core.applet.anchor {
            PanelAnchor::Top | PanelAnchor::Bottom => true,
            PanelAnchor::Left | PanelAnchor::Right => false,
        };
        let divider_padding = match self.core.applet.size {
            Size::Hardcoded(_) => 4,
            Size::PanelSize(ref s) => {
                let size = s.get_applet_icon_size_with_padding(false);

                let small_size_threshold = PanelSize::S.get_applet_icon_size_with_padding(false);

                if size <= small_size_threshold { 4 } else { 8 }
            }
        };
        let (favorite_popup_cutoff, active_popup_cutoff) = self.panel_overflow_lengths();

        // Only pinned launchers whose app has *no* active windows are shown.
        let visible_pinned: Vec<_> = self
            .pinned_list
            .iter()
            .filter(|item| !self.pinned_has_active_window(&item.original_app_id))
            .collect();
        let visible_pinned_len = visible_pinned.len();

        let mut favorite_to_remove = match favorite_popup_cutoff {
            Some(cutoff) if cutoff < visible_pinned_len => visible_pinned_len - cutoff + 1,
            _ => 0,
        };
        let favorites: Vec<_> = visible_pinned
            .iter()
            .rev()
            .filter(|f| {
                if favorite_to_remove > 0 && f.toplevels.is_empty() {
                    favorite_to_remove -= 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        let mut favorites: Vec<_> = favorites[favorite_to_remove..]
            .iter()
            .rev()
            .map(|dock_item| {
                let filtered_is_focused = dock_item
                    .toplevels
                    .iter()
                    .filter(|(info, _)| self.is_on_current_monitor_and_workspace(info))
                    .any(|y| focused_item.contains(&y.0.foreign_toplevel));

                self.core
                    .applet
                    .applet_tooltip::<Message>(
                        dock_item.as_icon(
                            &self.core.applet,
                            self.rectangle_tracker.as_ref(),
                            self.popup.is_none(),
                            self.gpus.as_deref(),
                            filtered_is_focused,
                            dot_radius,
                            self.core.main_window_id().unwrap(),
                            Some(&|info| self.is_on_current_monitor_and_workspace(info)),
                            DragSource::Favorites,
                        ),
                        dock_item.tooltip_text(&self.locales).into_owned(),
                        self.popup.is_some(),
                        Message::Surface,
                        None,
                    )
                    .into()
            })
            .collect();

        if favorite_popup_cutoff.is_some() {
            // button to show more favorites
            let icon = match self.core.applet.anchor {
                PanelAnchor::Bottom => "go-up-symbolic",
                PanelAnchor::Left => "go-next-symbolic",
                PanelAnchor::Right => "go-previous-symbolic",
                PanelAnchor::Top => "go-down-symbolic",
            };
            let btn = self
                .core
                .applet
                .icon_button(icon)
                .on_press(Message::OpenFavorites);
            let btn: Element<_> = if let Some(rectangle_tracker) = self.rectangle_tracker.as_ref() {
                rectangle_tracker
                    .container(DockItemId::FavoritesOverflow, btn)
                    .into()
            } else {
                btn.into()
            };
            favorites.push(btn);
        }

        let filtered_active_list: Vec<_> = self
            .active_list
            .iter()
            .filter(|dock_item| {
                dock_item.toplevels.iter().any(|(toplevel_info, _)| {
                    self.is_on_current_monitor_and_workspace(toplevel_info)
                })
            })
            .collect();

        let mut active: Vec<_> =
            filtered_active_list[..active_popup_cutoff.map_or(filtered_active_list.len(), |n| {
                if n < filtered_active_list.len() {
                    n.saturating_sub(1)
                } else {
                    n
                }
            })]
                .iter()
                .map(|dock_item| {
                    let filtered_is_focused = dock_item
                        .toplevels
                        .iter()
                        .filter(|(info, _)| self.is_on_current_monitor_and_workspace(info))
                        .any(|y| focused_item.contains(&y.0.foreign_toplevel));

                    self.core
                        .applet
                        .applet_tooltip(
                            dock_item.as_icon(
                                &self.core.applet,
                                self.rectangle_tracker.as_ref(),
                                self.popup.is_none(),
                                self.gpus.as_deref(),
                                filtered_is_focused,
                                dot_radius,
                                self.core.main_window_id().unwrap(),
                                Some(&|info| self.is_on_current_monitor_and_workspace(info)),
                                DragSource::Active,
                            ),
                            dock_item.tooltip_text(&self.locales).into_owned(),
                            self.popup.is_some(),
                            Message::Surface,
                            None,
                        )
                        .into()
                })
            .collect();

        if active_popup_cutoff.is_some_and(|n| n < filtered_active_list.len()) {
            // button to show more active
            let icon = match self.core.applet.anchor {
                PanelAnchor::Bottom => "go-up-symbolic",
                PanelAnchor::Left => "go-next-symbolic",
                PanelAnchor::Right => "go-previous-symbolic",
                PanelAnchor::Top => "go-down-symbolic",
            };
            let btn = self
                .core
                .applet
                .icon_button(icon)
                .on_press(Message::OpenActive);
            let btn: Element<_> = if let Some(rectangle_tracker) = self.rectangle_tracker.as_ref() {
                rectangle_tracker
                    .container(DockItemId::ActiveOverflow, btn)
                    .into()
            } else {
                btn.into()
            };
            active.push(btn);
        }

        let window_size = self.core.applet.suggested_bounds.as_ref();
        let max_num = if self.core.applet.is_horizontal() {
            let suggested_width = self.core.applet.suggested_size(false).0
                + self.core.applet.suggested_padding(false).0 * 2;
            window_size
                .map(|w| w.width)
                .map_or(u32::MAX, |b| (b / suggested_width as f32) as u32) as usize
        } else {
            let suggested_height = self.core.applet.suggested_size(false).1
                + self.core.applet.suggested_padding(false).0 * 2;
            window_size
                .map(|w| w.height)
                .map_or(u32::MAX, |b| (b / suggested_height as f32) as u32) as usize
        }
        .max(4);
        if max_num < favorites.len() + active.len() {
            let active_leftover = max_num.saturating_sub(favorites.len());
            favorites.truncate(max_num - active_leftover);
            active.truncate(active_leftover);
        }
        let (w, h, favorites, active, divider): (Length, Length, Element<'_, Message>, Element<'_, Message>, _) = if is_horizontal {
            (
                Length::Shrink,
                Length::Shrink,
                row(favorites).spacing(app_icon.icon_spacing).into(),
                row(active).spacing(app_icon.icon_spacing).into(),
                container(vertical_rule(1))
                    .height(Length::Fill)
                    .padding([divider_padding, 0])
                    .into(),
            )
        } else {
            (
                Length::Shrink,
                Length::Shrink,
                column(favorites).spacing(app_icon.icon_spacing).into(),
                column(active).spacing(app_icon.icon_spacing).into(),
                container(divider::horizontal::default())
                    .width(Length::Fill)
                    .padding([0, divider_padding])
                    .into(),
            )
        };

        let show_pinned = !self.pinned_list.is_empty();
        let content_list: Vec<Element<_>> = if show_pinned && !self.active_list.is_empty() {
            vec![favorites.into(), divider, active.into()]
        } else if show_pinned {
            vec![favorites.into()]
        } else if !self.active_list.is_empty() {
            vec![active.into()]
        } else {
            vec![
                icon::from_name("com.system76.CosmicWinList")
                    .size(self.core.applet.suggested_size(false).0)
                    .into(),
            ]
        };

        let mut content = match &self.core.applet.anchor {
            PanelAnchor::Left | PanelAnchor::Right => container(
                Column::with_children(content_list)
                    .spacing(4.0)
                    .align_x(Alignment::Center)
                    .height(h)
                    .width(w),
            ),
            PanelAnchor::Top | PanelAnchor::Bottom => container(
                Row::with_children(content_list)
                    .spacing(4.0)
                    .align_y(Alignment::Center)
                    .height(h)
                    .width(w),
            ),
        };
        if self.active_list.is_empty() && self.pinned_list.is_empty() {
            let suggested_size = self.core.applet.suggested_size(false);
            content = content.width(suggested_size.0).height(suggested_size.1);
        }

        let mut limits = Limits::NONE.min_width(1.).min_height(1.);

        if let Some(b) = self.core.applet.suggested_bounds {
            if b.width as i32 > 0 {
                limits = limits.max_width(b.width);
            }
            if b.height as i32 > 0 {
                limits = limits.max_height(b.height);
            }
        }

        self.core
            .applet
            .autosize_window(content)
            .limits(limits)
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        let theme = self.core.system_theme();

        if let Some(Popup {
            dock_item: DockItem { id, .. },
            popup_type,
            ..
        }) = self.popup.as_ref().filter(|p| id == p.id)
        {
            let (dock_item, is_pinned) = match self.pinned_list.iter().find(|i| i.id == *id) {
                Some(e) => (e, true),
                None => match self.active_list.iter().find(|i| i.id == *id) {
                    Some(e) => (e, false),
                    None => return text::body("").into(),
                },
            };

            // Filter toplevels to only show windows on current monitor and workspace
            let filtered_toplevels: Vec<_> = dock_item
                .toplevels
                .iter()
                .filter(|(toplevel_info, _)| {
                    self.is_on_current_monitor_and_workspace(toplevel_info)
                })
                .collect();

            let toplevels = &filtered_toplevels;
            let desktop_info = &dock_item.desktop_info;

            match popup_type {
                PopupType::RightClickMenu => {
                    fn menu_button<'a, Message: Clone + 'a>(
                        content: impl Into<Element<'a, Message>>,
                    ) -> cosmic::widget::Button<'a, Message> {
                        button::custom(content)
                            .height(20 + 2 * theme::spacing().space_xxs)
                            .class(Button::MenuItem)
                            .padding(menu_control_padding())
                            .width(Length::Fill)
                    }

                    let mut content =
                        menu::menu_column::MenuColumn::with_capacity(4).align_x(Alignment::Center);

                    if let Some(exec) = desktop_info.exec() {
                        if !toplevels.is_empty() {
                            content =
                                content.push(menu_button(text::body(fl!("new-window"))).on_press(
                                    Message::Exec(exec.to_string(), None, desktop_info.terminal()),
                                ));
                        } else if let Some(gpus) = self.gpus.as_ref() {
                            let default_idx = preferred_gpu_idx(desktop_info, gpus.iter());
                            for (i, gpu) in gpus.iter().enumerate() {
                                content = content.push(
                                    menu_button(text::body(format!(
                                        "{} {}",
                                        fl!("run-on", gpu = gpu.name.clone()),
                                        if i == default_idx {
                                            fl!("run-on-default")
                                        } else {
                                            String::new()
                                        }
                                    )))
                                    .on_press(Message::Exec(
                                        exec.to_string(),
                                        Some(i),
                                        desktop_info.terminal(),
                                    )),
                                );
                            }
                        } else {
                            content = content.push(menu_button(text::body(fl!("run"))).on_press(
                                Message::Exec(exec.to_string(), None, desktop_info.terminal()),
                            ));
                        }
                        for action in desktop_info.actions().into_iter().flatten() {
                            if action == "new-window" {
                                continue;
                            }

                            let Some(exec) = desktop_info.action_entry(action, "Exec") else {
                                continue;
                            };
                            let Some(name) =
                                desktop_info.action_entry_localized(action, "Name", &self.locales)
                            else {
                                continue;
                            };
                            content = content.push(menu_button(text::body(name)).on_press(
                                Message::Exec(exec.into(), None, desktop_info.terminal()),
                            ));
                        }
                        content = content.push(divider::horizontal::light());
                    }

                    if !toplevels.is_empty() {
                        let mut list_col = column![];
                        for (info, _) in toplevels {
                            list_col = list_col.push(
                                menu_button(
                                    text::body(&info.title)
                                        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                                )
                                .on_press(Message::Activate(info.foreign_toplevel.clone())),
                            );
                        }
                        content = content.push(list_col);
                        content = content.push(divider::horizontal::light());
                    }

                    let svg_accent = Rc::new(|theme: &cosmic::Theme| {
                        let color = theme.cosmic().accent_color().into();
                        svg::Style { color: Some(color) }
                    });
                    content = content.push(
                        menu_button(
                            if is_pinned {
                                row![
                                    icon::icon(from_name("checkbox-checked-symbolic").into())
                                        .size(16)
                                        .class(cosmic::theme::Svg::Custom(svg_accent.clone())),
                                    text::body(fl!("pin"))
                                ]
                            } else {
                                row![text::body(fl!("pin"))]
                            }
                            .spacing(8),
                        )
                        .on_press(if is_pinned {
                            Message::UnpinApp(*id)
                        } else {
                            Message::PinApp(*id)
                        }),
                    );

                    if !toplevels.is_empty() {
                        content = content.push(divider::horizontal::light());
                        content = match toplevels.len() {
                            1 => content.push(
                                menu_button(text::body(fl!("quit")))
                                    .on_press(Message::Quit(*id)),
                            ),
                            _ => content.push(
                                menu_button(text::body(fl!("quit-all")))
                                    .on_press(Message::Quit(*id)),
                            ),
                        };
                    }
                    self.core
                        .applet
                        .popup_container(
                            container(content)
                                .padding(1)
                                .height(Length::Shrink)
                                .width(Length::Fill),
                        )
                        .limits(
                            Limits::NONE
                                .min_width(1.)
                                .min_height(1.)
                                .max_width(300.)
                                .max_height(1000.),
                        )
                        .into()
                }
                PopupType::ToplevelList => match self.core.applet.anchor {
                    PanelAnchor::Left | PanelAnchor::Right => {
                        let mut content =
                            column![].padding(8).align_x(Alignment::Center).spacing(8);
                        for (info, img) in toplevels {
                            content = content.push(toplevel_button(
                                img.clone(),
                                info.title.clone(),
                                info.foreign_toplevel.clone(),
                                self.is_focused(&info.foreign_toplevel),
                                self.is_hovered(&info.foreign_toplevel),
                            ));
                        }
                        self.core
                            .applet
                            .popup_container(content)
                            .limits(Limits::NONE.min_width(1.).min_height(1.).max_height(1000.))
                            .into()
                    }
                    PanelAnchor::Bottom | PanelAnchor::Top => {
                        let mut content = row![].padding(8).align_y(Alignment::Center).spacing(8);
                        for (info, img) in toplevels {
                            content = content.push(toplevel_button(
                                img.clone(),
                                info.title.clone(),
                                info.foreign_toplevel.clone(),
                                self.is_focused(&info.foreign_toplevel),
                                self.is_hovered(&info.foreign_toplevel),
                            ));
                        }
                        self.core
                            .applet
                            .popup_container(content)
                            .limits(Limits::NONE.min_width(1.).min_height(1.).max_height(1000.))
                            .into()
                    }
                },
            }
        } else if self
            .overflow_active_popup
            .as_ref()
            .is_some_and(|overflow_id| overflow_id == &id)
        {
            let (_favorite_popup_cutoff, active_popup_cutoff) = self.panel_overflow_lengths();

            let focused_item = self.currently_active_toplevel();
            let dot_radius = theme.cosmic().radius_xs();

            let filtered_active_list: Vec<_> = self
                .active_list
                .iter()
                .filter(|dock_item| {
                    dock_item.toplevels.iter().any(|(toplevel_info, _)| {
                        self.is_on_current_monitor_and_workspace(toplevel_info)
                    })
                })
                .collect();

            let active: Vec<_> = filtered_active_list
                .iter()
                .rev()
                .take(active_popup_cutoff.map_or(filtered_active_list.len(), |n| {
                    if n < filtered_active_list.len() {
                        filtered_active_list.len() - n + 1
                    } else {
                        0
                    }
                }))
                .map(|dock_item| {
                    let filtered_is_focused = dock_item
                        .toplevels
                        .iter()
                        .filter(|(info, _)| self.is_on_current_monitor_and_workspace(info))
                        .any(|y| focused_item.contains(&y.0.foreign_toplevel));

                    self.core
                        .applet
                        .applet_tooltip(
                            dock_item.as_icon(
                                &self.core.applet,
                                self.rectangle_tracker.as_ref(),
                                self.popup.is_none(),
                                self.gpus.as_deref(),
                                filtered_is_focused,
                                dot_radius,
                                id,
                                Some(&|info| self.is_on_current_monitor_and_workspace(info)),
                                DragSource::Active,
                            ),
                            dock_item.tooltip_text(&self.locales).into_owned(),
                            self.popup.is_some(),
                            Message::Surface,
                            Some(id),
                        )
                        .into()
                })
                .collect();
            let content = match &self.core.applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => container(
                    Column::with_children(active)
                        .spacing(4.0)
                        .align_x(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
                PanelAnchor::Top | PanelAnchor::Bottom => container(
                    Row::with_children(active)
                        .spacing(4.0)
                        .align_y(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
            };
            // send clear popup on press content if there is an active popup
            let content: Element<_> = if self.popup.is_some() {
                mouse_area(content)
                    .on_release(Message::ClosePopup)
                    .on_right_release(Message::ClosePopup)
                    .into()
            } else {
                content.into()
            };
            self.core
                .applet
                .popup_container(content)
                .limits(
                    Limits::NONE
                        .min_width(1.)
                        .min_height(1.)
                        .max_width(1920.)
                        .max_height(1000.),
                )
                .into()
        } else if self
            .overflow_favorites_popup
            .as_ref()
            .is_some_and(|popup_id| popup_id == &id)
        {
            let (favorite_popup_cutoff, _active_popup_cutoff) = self.panel_overflow_lengths();

            let focused_item = self.currently_active_toplevel();
            let dot_radius = theme.cosmic().radius_xs();

            let visible_pinned: Vec<_> = self
                .pinned_list
                .iter()
                .filter(|item| !self.pinned_has_active_window(&item.original_app_id))
                .collect();
            let visible_pinned_len = visible_pinned.len();

            let overflow_favorites = match favorite_popup_cutoff {
                Some(cutoff) if cutoff < visible_pinned_len => &visible_pinned[cutoff..],
                _ => &[],
            };

            let favorites: Vec<_> = overflow_favorites
                .iter()
                .map(|dock_item| {
                    let filtered_is_focused = dock_item
                        .toplevels
                        .iter()
                        .filter(|(info, _)| self.is_on_current_monitor_and_workspace(info))
                        .any(|y| focused_item.contains(&y.0.foreign_toplevel));

                    self.core
                        .applet
                        .applet_tooltip(
                        dock_item.as_icon(
                            &self.core.applet,
                            self.rectangle_tracker.as_ref(),
                            self.popup.is_none(),
                            self.gpus.as_deref(),
                            filtered_is_focused,
                            dot_radius,
                            id,
                            Some(&|info| self.is_on_current_monitor_and_workspace(info)),
                            DragSource::Favorites,
                        ),
                        dock_item.tooltip_text(&self.locales).to_string(),
                        self.popup.is_some(),
                        Message::Surface,
                        Some(id),
                    )
                    .into()
                })
                .collect();
            let content = match &self.core.applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => container(
                    Column::with_children(favorites)
                        .spacing(4.0)
                        .align_x(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
                PanelAnchor::Top | PanelAnchor::Bottom => container(
                    Row::with_children(favorites)
                        .spacing(4.0)
                        .align_y(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
            };
            let content: Element<_> = if self.popup.is_some() {
                mouse_area(content)
                    .on_right_release(Message::ClosePopup)
                    .on_press(Message::ClosePopup)
                    .into()
            } else {
                content.into()
            };
            self.core
                .applet
                .popup_container(content)
                .limits(
                    Limits::NONE
                        .min_width(1.)
                        .min_height(1.)
                        .max_width(1920.)
                        .max_height(1000.),
                )
                .into()
        } else {
            let suggested = self.core.applet.suggested_size(false);
            iced::widget::row!()
                .width(Length::Fixed(suggested.0 as f32))
                .height(Length::Fixed(suggested.1 as f32))
                .into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            wayland_subscription().map(Message::Wayland),
            listen_with(|e, _, id| match e {
                cosmic::iced::core::Event::PlatformSpecific(event::PlatformSpecific::Wayland(
                    event::wayland::Event::Seat(e, seat),
                )) => match e {
                    event::wayland::SeatEvent::Enter => Some(Message::NewSeat(seat)),
                    event::wayland::SeatEvent::Leave => Some(Message::RemovedSeat),
                },
                cosmic::iced::core::Event::Mouse(
                    cosmic::iced::core::mouse::Event::ButtonPressed(_),
                ) => Some(Message::Pressed(id)),
                cosmic::iced::core::Event::Mouse(
                    cosmic::iced::core::mouse::Event::CursorMoved { position },
                ) => {
                    Some(Message::DragMove(
                        iced::Point::new(position.x, position.y),
                    ))
                }
                _ => None,
            }),
            rectangle_tracker_subscription(0).map(|update| Message::Rectangle(update.1)),
            self.core.watch_config(APP_ID).map(|u| {
                for why in u.errors {
                    tracing::error!(why = why.to_string(), "Error watching config");
                }
                Message::ConfigUpdated(u.config)
            }),
        ])
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

fn launch_on_preferred_gpu(desktop_info: &DesktopEntry, gpus: Option<&[Gpu]>) -> Option<Message> {
    let exec = desktop_info.exec()?;

    let gpu_idx = gpus.map(|gpus| preferred_gpu_idx(desktop_info, gpus.iter()));

    Some(Message::Exec(
        exec.to_string(),
        gpu_idx,
        desktop_info.terminal(),
    ))
}

fn preferred_gpu_idx<'a, I>(desktop_info: &DesktopEntry, mut gpus: I) -> usize
where
    I: Iterator<Item = &'a Gpu>,
{
    gpus.position(|gpu| gpu.default ^ desktop_info.prefers_non_default_gpu())
        .unwrap_or(0)
}
