use crate::worker::{self, Command, Update};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tokio::sync::mpsc;
use waveflow_native::{backend::TrackView, PlayerState};

struct Row {
    id: i64,
    title: String,
    detail: String,
    playlist: bool,
}

struct ViewState {
    page: usize,
    view: TrackView,
    generation: u64,
    ids: Vec<i64>,
}

fn send(tx: &mpsc::Sender<Command>, toast: &adw::ToastOverlay, command: Command) {
    if let Err(err) = tx.try_send(command) {
        let message = match err {
            mpsc::error::TrySendError::Full(_) => {
                "Le lecteur est occupé, réessayez dans un instant."
            }
            mpsc::error::TrySendError::Closed(_) => "Le service du lecteur est arrêté.",
        };
        toast.add_toast(adw::Toast::new(message));
    }
}

fn button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn duration(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

pub fn build(app: &adw::Application) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("WaveFlow")
        .default_width(1000)
        .default_height(720)
        .width_request(620)
        .height_request(440)
        .build();
    let toast = adw::ToastOverlay::new();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("WaveFlow", "")));
    layout.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_width_request(150);
    for name in [
        "Accueil",
        "Bibliothèque",
        "Playlists",
        "Favoris",
        "Historique",
        "Paramètres",
    ] {
        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_margin_top(12);
        label.set_margin_bottom(12);
        label.set_margin_start(12);
        label.set_margin_end(12);
        sidebar.append(&label);
    }
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .child(&sidebar)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    body.append(&sidebar_scroll);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_hexpand(true);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let heading = gtk::Label::new(Some("Accueil"));
    heading.set_xalign(0.0);
    heading.add_css_class("title-1");
    content.append(&heading);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Rechercher un morceau")
        .build();
    search.update_property(&[gtk::accessible::Property::Label(
        "Rechercher dans la bibliothèque",
    )]);
    content.append(&search);
    let status = gtk::Label::new(Some("Ouverture de la bibliothèque…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    content.append(&status);

    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let detail = gtk::Label::new(None);
        detail.set_xalign(0.0);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        detail.add_css_class("dim-label");
        row.append(&title);
        row.append(&detail);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let object = item
            .item()
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        let value = object.borrow::<Row>();
        let row = item.child().unwrap().downcast::<gtk::Box>().unwrap();
        let title = row.first_child().unwrap().downcast::<gtk::Label>().unwrap();
        let detail = title
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        title.set_label(&value.title);
        detail.set_label(&value.detail);
        row.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} — {}",
            value.title, value.detail
        ))]);
    });
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.set_single_click_activate(false);
    list.update_property(&[gtk::accessible::Property::Label("Morceaux")]);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&scroll, Some("tracks"));
    let settings = gtk::Label::new(Some("Chargement du profil…"));
    settings.set_wrap(true);
    settings.set_selectable(true);
    settings.set_xalign(0.0);
    settings.set_yalign(0.0);
    stack.add_named(&settings, Some("settings"));
    content.append(&stack);
    body.append(&content);
    layout.append(&body);
    layout.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let player = gtk::Box::new(gtk::Orientation::Vertical, 8);
    player.set_margin_top(12);
    player.set_margin_bottom(12);
    player.set_margin_start(16);
    player.set_margin_end(16);
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let cover = gtk::Picture::new();
    cover.set_size_request(56, 56);
    cover.set_can_shrink(true);
    cover.update_property(&[gtk::accessible::Property::Label(
        "Pochette du morceau courant",
    )]);
    transport.append(&cover);
    let title = gtk::Label::new(Some("Aucun morceau"));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(8);
    transport.append(&title);
    let previous = button("media-skip-backward-symbolic", "Morceau précédent");
    let play = button("media-playback-start-symbolic", "Lecture / pause");
    let next = button("media-skip-forward-symbolic", "Morceau suivant");
    for b in [&previous, &play, &next] {
        b.set_sensitive(false);
        transport.append(b);
    }
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    volume.set_width_request(95);
    volume.set_draw_value(false);
    volume.set_tooltip_text(Some("Volume"));
    volume.update_property(&[gtk::accessible::Property::Label("Volume")]);
    volume.set_sensitive(false);
    transport.append(&volume);
    player.append(&transport);
    let timeline = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let position = gtk::Label::new(Some("0:00"));
    let total = gtk::Label::new(Some("0:00"));
    let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    progress.set_draw_value(false);
    progress.set_hexpand(true);
    progress.set_sensitive(false);
    progress.update_property(&[gtk::accessible::Property::Label("Position de lecture")]);
    timeline.append(&position);
    timeline.append(&progress);
    timeline.append(&total);
    player.append(&timeline);
    layout.append(&player);
    toast.set_child(Some(&layout));
    window.set_content(Some(&toast));

    let (tx, mut updates) = worker::start();
    let view = Rc::new(RefCell::new(ViewState {
        page: 0,
        view: TrackView::Library,
        generation: 0,
        ids: Vec::new(),
    }));
    let ready = Rc::new(Cell::new(false));
    let reload: Rc<dyn Fn()> = {
        let (view, tx, toast, search, status, ready) = (
            view.clone(),
            tx.clone(),
            toast.clone(),
            search.clone(),
            status.clone(),
            ready.clone(),
        );
        Rc::new(move || {
            if !ready.get() {
                return;
            }
            let mut state = view.borrow_mut();
            state.generation += 1;
            let generation = state.generation;
            if state.page == 5 {
                return;
            }
            status.set_label("Chargement…");
            let command = if state.page == 2 {
                Command::Playlists { generation }
            } else {
                Command::Tracks {
                    generation,
                    view: state.view.clone(),
                    query: search.text().to_string(),
                }
            };
            send(&tx, &toast, command);
        })
    };
    sidebar.connect_row_selected({
        let (view, reload, heading, stack, search, status) = (
            view.clone(),
            reload.clone(),
            heading.clone(),
            stack.clone(),
            search.clone(),
            status.clone(),
        );
        move |_, row| {
            let Some(row) = row else { return };
            let page = row.index() as usize;
            {
                let mut state = view.borrow_mut();
                state.page = page;
                state.view = match page {
                    3 => TrackView::Favorites,
                    4 => TrackView::History,
                    _ => TrackView::Library,
                };
            }
            heading.set_label(
                [
                    "Accueil",
                    "Bibliothèque",
                    "Playlists",
                    "Favoris",
                    "Historique",
                    "Paramètres",
                ][page],
            );
            stack.set_visible_child_name(if page == 5 { "settings" } else { "tracks" });
            search.set_visible(page != 5 && page != 2);
            status.set_visible(page != 5);
            reload();
        }
    });
    search.connect_search_changed({
        let reload = reload.clone();
        move |_| reload()
    });
    list.connect_activate({
        let (view, model, tx, toast, reload, heading, search) = (
            view.clone(),
            model.clone(),
            tx.clone(),
            toast.clone(),
            reload.clone(),
            heading.clone(),
            search.clone(),
        );
        move |_, index| {
            let Some(object) = model.item(index).and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = object.borrow::<Row>();
            if row.playlist {
                let mut state = view.borrow_mut();
                state.page = 6;
                state.view = TrackView::Playlist(row.id);
                drop(state);
                heading.set_label(&row.title);
                search.set_visible(true);
                reload();
            } else {
                send(
                    &tx,
                    &toast,
                    Command::Play {
                        ids: view.borrow().ids.clone(),
                        index: index as usize,
                    },
                );
            }
        }
    });
    for (name, b, shortcut) in [
        ("toggle", play.clone(), "<Primary>space"),
        ("previous", previous.clone(), "<Alt>Left"),
        ("next", next.clone(), "<Alt>Right"),
    ] {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate({
            let (tx, toast) = (tx.clone(), toast.clone());
            move |_, _| {
                send(
                    &tx,
                    &toast,
                    match name {
                        "toggle" => Command::Toggle,
                        "previous" => Command::Previous,
                        _ => Command::Next,
                    },
                )
            }
        });
        app.add_action(&action);
        app.set_accels_for_action(&format!("app.{name}"), &[shortcut]);
        b.set_action_name(Some(&format!("app.{name}")));
    }
    let find = gio::SimpleAction::new("search", None);
    find.connect_activate({
        let search = search.clone();
        move |_, _| {
            search.grab_focus();
        }
    });
    app.add_action(&find);
    app.set_accels_for_action("app.search", &["<Primary>f"]);
    progress.connect_change_value({
        let (tx, toast) = (tx.clone(), toast.clone());
        move |_, _, value| {
            send(&tx, &toast, Command::Seek(value.max(0.0) as u64));
            glib::Propagation::Proceed
        }
    });
    volume.connect_change_value({
        let (tx, toast) = (tx.clone(), toast.clone());
        move |_, _, value| {
            send(&tx, &toast, Command::Volume((value / 100.0) as f32));
            glib::Propagation::Proceed
        }
    });
    let shutting_down = Rc::new(Cell::new(false));
    let stopped = Rc::new(Cell::new(false));
    window.connect_close_request({
        let tx = tx.clone();
        let stopped = stopped.clone();
        move |window| {
            if stopped.get() || tx.is_closed() {
                return glib::Propagation::Proceed;
            }
            if !shutting_down.replace(true) {
                window.set_sensitive(false);
                let tx = tx.clone();
                glib::spawn_future_local(async move {
                    let _ = tx.send(Command::Shutdown).await;
                });
            }
            glib::Propagation::Stop
        }
    });
    let weak_window = window.downgrade();
    glib::spawn_future_local(async move {
        while let Some(update) = updates.recv().await {
            if weak_window.upgrade().is_none() {
                break;
            }
            match update {
                Update::Stopped => {
                    stopped.set(true);
                    if let Some(window) = weak_window.upgrade() {
                        window.close();
                    }
                    break;
                }
                Update::Ready { profile, root } => {
                    ready.set(true);
                    for b in [&previous, &play, &next] {
                        b.set_sensitive(true);
                    }
                    volume.set_sensitive(true);
                    settings.set_label(&format!(
                        "{profile}\n\nBibliothèque : {}\n\nApparence : préférence du système",
                        root.display()
                    ));
                    reload();
                }
                Update::Tracks { generation, rows } => {
                    let mut state = view.borrow_mut();
                    if state.generation != generation {
                        continue;
                    }
                    state.ids = rows.iter().map(|row| row.id).collect();
                    let objects: Vec<_> = rows
                        .into_iter()
                        .map(|row| {
                            glib::BoxedAnyObject::new(Row {
                                id: row.id,
                                title: row.title,
                                detail: format!(
                                    "{} · {} · {}",
                                    row.artist_name.as_deref().unwrap_or("Artiste inconnu"),
                                    row.album_title.as_deref().unwrap_or("Album inconnu"),
                                    duration(row.duration_ms.max(0) as u64)
                                ),
                                playlist: false,
                            })
                        })
                        .collect();
                    model.splice(0, model.n_items(), &objects);
                    status.set_label(if objects.is_empty() {
                        "Aucun morceau. Ajoutez votre musique avec le scanner WaveFlow."
                    } else {
                        "Double-cliquez sur un morceau, ou appuyez sur Entrée pour le lire."
                    });
                }
                Update::Playlists { generation, rows } => {
                    if view.borrow().generation != generation {
                        continue;
                    }
                    let objects: Vec<_> = rows
                        .into_iter()
                        .map(|row| {
                            glib::BoxedAnyObject::new(Row {
                                id: row.id,
                                title: row.name,
                                detail: format!("{} morceaux", row.track_count),
                                playlist: true,
                            })
                        })
                        .collect();
                    model.splice(0, model.n_items(), &objects);
                    status.set_label(if objects.is_empty() {
                        "Aucune playlist dans ce profil."
                    } else {
                        "Ouvrez une playlist pour afficher ses morceaux."
                    });
                }
                Update::Playback(live) => {
                    play.set_icon_name(if live.state == PlayerState::Playing {
                        "media-playback-pause-symbolic"
                    } else {
                        "media-playback-start-symbolic"
                    });
                    position.set_label(&duration(live.position_ms));
                    progress.set_value(live.position_ms as f64);
                    volume.set_value(f64::from(live.volume) * 100.0);
                }
                Update::Current { row, artwork } => {
                    if let Some(row) = row {
                        title.set_label(&format!(
                            "{} — {}",
                            row.title,
                            row.artist_name.as_deref().unwrap_or("Artiste inconnu")
                        ));
                        total.set_label(&duration(row.duration_ms.max(0) as u64));
                        progress.set_range(0.0, row.duration_ms.max(1) as f64);
                        progress.set_sensitive(row.duration_ms > 0);
                    } else {
                        title.set_label("Aucun morceau");
                        total.set_label("0:00");
                        progress.set_sensitive(false);
                    }
                    if let Some(art) = artwork {
                        let bytes = glib::Bytes::from_owned(art.rgba);
                        let texture = gtk::gdk::MemoryTexture::new(
                            art.width as i32,
                            art.height as i32,
                            gtk::gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            art.width as usize * 4,
                        );
                        cover.set_paintable(Some(&texture));
                    } else {
                        cover.set_paintable(None::<&gtk::gdk::Texture>);
                    }
                }
                Update::Error(message) => {
                    if !ready.get() {
                        status.set_label(&message);
                    }
                    toast.add_toast(adw::Toast::new(&message));
                }
            }
        }
    });
    sidebar.select_row(sidebar.row_at_index(0).as_ref());
    window.present();
}
