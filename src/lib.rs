use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use mod_api::text::TextContent;
use mod_api::{
    self, ClientHost, DebugEntry, DebugEntryLinesEvent, DebugGroup, Flow, GuiRenderPostEvent, Hand,
    Hooks, ItemStack, ItemTooltipEvent, PayloadId, PayloadSendError, PlayerLoggedInEvent,
    RegisterDebugEntriesEvent, Registrar, RegistrationContext, RenderTooltipPreEvent, ServerHost,
    ServerTickEvent, ServerTickPhase, Tag, Text, ToClient, TooltipElement,
};

const SATURATION_TEXTURE: &str = "appleskin:gui/sprites/icons";
const FOOD_TOOLTIP_ROW: &str = "                        ";
const SATURATION_TOOLTIP_ROW: &str = "                         ";
const TOOLTIP_ICON_SPACING: i32 = 8;
const TOOLTIP_ICON_SIZE: i32 = 9;
const TOOLTIP_ICON_COUNT: i32 = 10;

#[derive(Default)]
struct AppleSkin {
    pending_tooltip: Mutex<Option<Vec<SpriteDraw>>>,
    synced_saturation: Arc<Mutex<Option<f32>>>,
    saturation_payload: Mutex<Option<ToClient<f32>>>,
    last_server_saturation: Mutex<HashMap<u128, f32>>,
    reported_saturation_errors: Mutex<HashSet<u128>>,
}

#[mod_api::horizon_mod]
impl Hooks for AppleSkin {
    fn register(&self, registrar: &mut dyn Registrar, _: &RegistrationContext) {
        let client_saturation = Arc::clone(&self.synced_saturation);
        let payload = registrar
            .payload_handlers()
            .registrar("1")
            .optional()
            .play_to_client_typed::<f32>(
                PayloadId("appleskin:saturation".to_owned()),
                move |saturation, _| {
                    if !saturation.is_finite() || !(0.0..=20.0).contains(&saturation) {
                        return Err("received invalid saturation value".to_owned());
                    }
                    *client_saturation.lock().unwrap() = Some(saturation);
                    Ok(())
                },
            );
        *self.saturation_payload.lock().unwrap() = Some(payload);
    }

    fn player_logged_in(&self, _: &mut dyn ServerHost, event: &mut PlayerLoggedInEvent<'_>) {
        self.last_server_saturation
            .lock()
            .unwrap()
            .remove(&event.player);
        self.reported_saturation_errors
            .lock()
            .unwrap()
            .remove(&event.player);
    }

    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {
        if !matches!(event.phase, ServerTickPhase::ServerTickEnd { .. }) {
            return;
        }

        let Some(payload) = self.saturation_payload.lock().unwrap().clone() else {
            return;
        };
        let players = host.players().server_players();
        let online_players: HashSet<_> = players.iter().map(|player| player.uuid()).collect();

        for player in players {
            let Some(food) = player.food(host) else {
                continue;
            };
            let saturation = food.saturation;
            if !saturation.is_finite() {
                continue;
            }

            let player_id = player.uuid();
            let changed = self
                .last_server_saturation
                .lock()
                .unwrap()
                .get(&player_id)
                .is_none_or(|last| last.to_bits() != saturation.to_bits());
            if !changed {
                continue;
            }

            match payload.send(host, player, &saturation) {
                Ok(_) | Err(PayloadSendError::UnsupportedPeer(_)) => {
                    self.last_server_saturation
                        .lock()
                        .unwrap()
                        .insert(player_id, saturation);
                    self.reported_saturation_errors
                        .lock()
                        .unwrap()
                        .remove(&player_id);
                }
                Err(error) => {
                    if self
                        .reported_saturation_errors
                        .lock()
                        .unwrap()
                        .insert(player_id)
                    {
                        eprintln!("AppleSkin could not sync player saturation: {error:?}");
                    }
                }
            }
        }

        self.last_server_saturation
            .lock()
            .unwrap()
            .retain(|player, _| online_players.contains(player));
        self.reported_saturation_errors
            .lock()
            .unwrap()
            .retain(|player| online_players.contains(player));
    }

    fn register_debug_entries(
        &self,
        _: &RegistrationContext,
        event: &mut RegisterDebugEntriesEvent<'_>,
    ) {
        event.entries.push(DebugEntry {
            id: "appleskin:food".to_owned(),
            group: DebugGroup::Position,
        });
    }

    fn debug_entry_lines(&self, host: &dyn ClientHost, event: &mut DebugEntryLinesEvent<'_>) {
        if event.id != "appleskin:food" {
            return;
        }

        let Some(player) = host.local_player() else {
            return;
        };
        let Some(food) = player.food(host) else {
            return;
        };

        event.lines.push(format!(
            "Food {} / 20, saturation {:.2}, exhaustion {:.2} / 4",
            food.level, food.saturation, food.exhaustion
        ));
    }

    fn item_tooltip(&self, host: &dyn ClientHost, event: &mut ItemTooltipEvent<'_>) {
        let Some(properties) = food_properties(host, event.stack) else {
            return;
        };

        if !properties.saturation.is_finite() {
            return;
        }
        event
            .lines
            .push((Text::literal(FOOD_TOOLTIP_ROW), u32::MAX));
        event
            .lines
            .push((Text::literal(SATURATION_TOOLTIP_ROW), u32::MAX));
    }

    fn render_tooltip_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut RenderTooltipPreEvent<'_>,
    ) -> Flow {
        let Some(stack) = event.stack else {
            *self.pending_tooltip.lock().unwrap() = None;
            return Flow::Continue;
        };
        let Some(properties) = food_properties(host, stack) else {
            *self.pending_tooltip.lock().unwrap() = None;
            return Flow::Continue;
        };
        if event.cancelled {
            *self.pending_tooltip.lock().unwrap() = None;
            return Flow::Continue;
        }

        let Some(draws) = tooltip_icon_draws(event, properties) else {
            *self.pending_tooltip.lock().unwrap() = None;
            return Flow::Continue;
        };
        *self.pending_tooltip.lock().unwrap() = Some(draws);
        Flow::Continue
    }

    fn gui_render_post(&self, host: &dyn ClientHost, event: &mut GuiRenderPostEvent<'_>) {
        let tooltip_draws = self
            .pending_tooltip
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        paint_food_overlay(event, &tooltip_draws);

        if event.gui_hidden || host.is_paused() {
            return;
        }

        let Some(player) = host.local_player() else {
            *self.synced_saturation.lock().unwrap() = None;
            return;
        };
        let Some(food) = player.food(host) else {
            *self.synced_saturation.lock().unwrap() = None;
            return;
        };

        let saturation = self
            .synced_saturation
            .lock()
            .unwrap()
            .unwrap_or(food.saturation);

        let [width, height] = event.batch.size();
        let (right, top) = food_bar_origin(width, height);
        let mut draws = Vec::with_capacity(32);

        if let Some(draw) = exhaustion_overlay(food.exhaustion, right, top) {
            draws.push(texture_draw(draw, 0xBFFFFFFF));
        }
        draws.extend(
            saturation_overlay(saturation, 0.0, right, top)
                .into_iter()
                .map(|draw| texture_draw(draw, 0xFFFFFFFF)),
        );

        let Some(living) = host.living(player.entity()) else {
            paint_food_overlay(event, &draws);
            return;
        };
        let Some(held) = living.hand_item(host, Hand::Main) else {
            paint_food_overlay(event, &draws);
            return;
        };
        let Some(properties) = food_properties(host, &held) else {
            paint_food_overlay(event, &draws);
            return;
        };
        if food.level >= 20 && !properties.can_always_eat {
            paint_food_overlay(event, &draws);
            return;
        }

        let preview = preview_food(food.level, saturation, properties);
        draws.extend(
            hunger_preview(food.level, preview.food, right, top)
                .into_iter()
                .flat_map(|icon| {
                    [
                        food_sprite("gui/sprites/hud/food_empty", icon, 0x40FFFFFF),
                        food_sprite(
                            if icon.half {
                                "gui/sprites/hud/food_half"
                            } else {
                                "gui/sprites/hud/food_full"
                            },
                            icon,
                            0xBFFFFFFF,
                        ),
                    ]
                }),
        );
        draws.extend(
            preview_saturation_overlay(saturation, preview, right, top)
                .into_iter()
                .map(|draw| texture_draw(draw, 0xBFFFFFFF)),
        );
        paint_food_overlay(event, &draws);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SpriteDraw {
    sprite: &'static str,
    src: [f32; 4],
    dst: [f32; 4],
    color: u32,
    missing_color: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextureDraw {
    src: [i32; 4],
    dst: [i32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HungerIcon {
    x: i32,
    y: i32,
    half: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FoodProperties {
    nutrition: u32,
    saturation: f32,
    can_always_eat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FoodPreview {
    food: i32,
    saturation_gained: f32,
}

fn food_bar_origin(width: f32, height: f32) -> (i32, i32) {
    (width as i32 / 2 + 91, height as i32 - 39)
}

fn saturation_overlay(
    saturation_level: f32,
    saturation_gained: f32,
    right: i32,
    top: i32,
) -> Vec<TextureDraw> {
    let total_saturation = saturation_level + saturation_gained;
    if !total_saturation.is_finite() || total_saturation < 0.0 {
        return Vec::new();
    }

    let saturation = total_saturation.clamp(0.0, 20.0);
    let start = if saturation_gained != 0.0 {
        (saturation_level / 2.0).max(0.0) as i32
    } else {
        0
    };
    let end = (saturation / 2.0).ceil() as i32;

    (start..end)
        .map(|index| {
            let fill = saturation / 2.0 - index as f32;
            let source_x = saturation_fill_source(fill);
            TextureDraw {
                src: [source_x, 0, 9, 9],
                dst: [right - index * 8 - 9, top, 9, 9],
            }
        })
        .collect()
}

fn preview_saturation_overlay(
    current_saturation: f32,
    preview: FoodPreview,
    right: i32,
    top: i32,
) -> Vec<TextureDraw> {
    saturation_overlay(current_saturation, preview.saturation_gained, right, top)
}

fn saturation_fill_source(fill: f32) -> i32 {
    if fill >= 1.0 {
        27
    } else if fill > 0.5 {
        18
    } else if fill > 0.25 {
        9
    } else {
        0
    }
}

fn exhaustion_overlay(exhaustion: f32, right: i32, top: i32) -> Option<TextureDraw> {
    if !exhaustion.is_finite() {
        return None;
    }
    let width = (exhaustion.clamp(0.0, 4.0) / 4.0 * 81.0) as i32;
    (width > 0).then_some(TextureDraw {
        src: [81 - width, 18, width, 1],
        dst: [right - width, top + 8, width, 1],
    })
}

fn hunger_preview(current_food: i32, food_gained: i32, right: i32, top: i32) -> Vec<HungerIcon> {
    if food_gained <= 0 {
        return Vec::new();
    }
    let food_after = current_food.saturating_add(food_gained).clamp(0, 20);
    let start = current_food.max(0) / 2;
    let end = (food_after + 1) / 2;
    (start..end)
        .map(|index| HungerIcon {
            x: right - index * 8 - 9,
            y: top,
            half: index * 2 + 1 == food_after,
        })
        .collect()
}

fn texture_draw(draw: TextureDraw, color: u32) -> SpriteDraw {
    SpriteDraw {
        sprite: SATURATION_TEXTURE,
        src: [
            draw.src[0] as f32,
            draw.src[1] as f32,
            draw.src[2] as f32,
            draw.src[3] as f32,
        ],
        dst: [
            draw.dst[0] as f32,
            draw.dst[1] as f32,
            draw.dst[2] as f32,
            draw.dst[3] as f32,
        ],
        color,
        missing_color: Some(0xDFFFC800),
    }
}

fn food_sprite(sprite: &'static str, icon: HungerIcon, color: u32) -> SpriteDraw {
    SpriteDraw {
        sprite,
        src: [0.0, 0.0, 9.0, 9.0],
        dst: [icon.x as f32, icon.y as f32, 9.0, 9.0],
        color,
        missing_color: None,
    }
}

fn paint_food_overlay(event: &mut GuiRenderPostEvent<'_>, draws: &[SpriteDraw]) {
    for draw in draws {
        if !event
            .batch
            .sprite(draw.sprite, draw.src, draw.dst, Some(draw.color))
            && let Some(color) = draw.missing_color
        {
            event.batch.fill(draw.dst, color);
        }
    }
}

fn tooltip_icon_draws(
    event: &mut RenderTooltipPreEvent<'_>,
    properties: FoodProperties,
) -> Option<Vec<SpriteDraw>> {
    let rows = tooltip_metric_rows(event.elements)?;
    let (width, height, offsets) = tooltip_element_layout(event.elements, &*event.draw)?;
    let place = (event.place.x as f32, event.place.y as f32);
    let screen = (event.screen_width as f32, event.screen_height as f32);
    let (left, top) = tooltip_position(place, screen, (width, height));
    let food_x = left;
    let saturation_x = left;
    let food_y = top + offsets[rows.0];
    let saturation_y = top + offsets[rows.1];
    Some(tooltip_value_icons(
        properties.nutrition.min(20),
        properties.saturation.clamp(0.0, 20.0),
        (food_x, food_y),
        (saturation_x, saturation_y),
    ))
}

fn tooltip_metric_rows(elements: &[TooltipElement]) -> Option<(usize, usize)> {
    let mut food = None;
    let mut saturation = None;
    for (index, element) in elements.iter().enumerate() {
        let TooltipElement::Text(text, _) = element else {
            continue;
        };
        let TextContent::Literal(line) = &text.content else {
            continue;
        };
        if line == FOOD_TOOLTIP_ROW {
            food = Some(index);
        } else if line == SATURATION_TOOLTIP_ROW {
            saturation = Some(index);
        }
    }
    Some((food?, saturation?))
}

fn tooltip_element_layout(
    elements: &[TooltipElement],
    draw: &dyn mod_api::DrawContext,
) -> Option<(f32, f32, Vec<f32>)> {
    let sizes = elements
        .iter()
        .map(|element| match element {
            TooltipElement::Text(text, _) => {
                let mut plain = String::new();
                text_for_measurement(text, &mut plain);
                [draw.text_width(&plain), 10.0]
            }
            TooltipElement::Image(image) => image.size,
        })
        .collect::<Vec<_>>();
    if sizes
        .iter()
        .flatten()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return None;
    }

    let mut height = if sizes.len() > 1 { 0.0 } else { -2.0 };
    let mut width = 0.0_f32;
    let mut top = 0.0;
    let mut offsets = Vec::with_capacity(sizes.len());
    for (index, size) in sizes.iter().enumerate() {
        offsets.push(top);
        width = width.max(size[0]);
        height += size[1];
        top += size[1]
            + if index == 0 && sizes.len() > 1 {
                2.0
            } else {
                0.0
            };
    }
    Some((width.ceil(), height.ceil(), offsets))
}

fn text_for_measurement(text: &Text, out: &mut String) {
    match &text.content {
        TextContent::Literal(value) => out.push_str(value),
        TextContent::Translatable {
            key,
            fallback,
            arguments,
        } => {
            if let Some(fallback) = fallback {
                out.push_str(fallback);
            } else {
                out.push_str(
                    key.rsplit('.')
                        .next()
                        .unwrap_or(key)
                        .replace('_', " ")
                        .as_str(),
                );
            }
            for argument in arguments {
                text_for_measurement(argument, out);
            }
        }
        TextContent::Keybind(key) => out.push_str(key),
        TextContent::Selector { pattern, .. } => out.push_str(pattern),
        TextContent::Score { name, .. } => out.push_str(name),
        TextContent::Nbt { path, .. } => out.push_str(path),
        TextContent::Object(_) => out.push(' '),
    }
    for child in &text.children {
        text_for_measurement(child, out);
    }
}

fn tooltip_position(
    (x, y): (f32, f32),
    (screen_width, screen_height): (f32, f32),
    (width, height): (f32, f32),
) -> (f32, f32) {
    let mut x = (x + 12.0).round();
    let mut y = (y - 12.0).round();
    if x + width > screen_width - 4.0 {
        x = (x - 28.0 - width).max(4.0);
    }
    if x + width > screen_width - 4.0 {
        x = (screen_width - width - 4.0).max(4.0);
    }
    if y + height + 3.0 > screen_height {
        y = screen_height - height - 3.0;
    }
    y = y.max(4.0);
    (x.round(), y.round())
}

fn tooltip_value_icons(
    nutrition: u32,
    saturation: f32,
    food_at: (f32, f32),
    saturation_at: (f32, f32),
) -> Vec<SpriteDraw> {
    let mut draws = Vec::with_capacity((TOOLTIP_ICON_COUNT * 4) as usize);
    let nutrition = nutrition.min(20) as i32;
    let food_fills = (nutrition + 1) / 2;
    let saturation = saturation.clamp(0.0, 20.0);
    let saturation_fills = (saturation / 2.0).ceil() as i32;

    for index in 0..TOOLTIP_ICON_COUNT {
        let x = index * TOOLTIP_ICON_SPACING;
        let food_icon = HungerIcon {
            x: food_at.0 as i32 + x,
            y: food_at.1 as i32,
            half: index * 2 + 1 == nutrition,
        };
        draws.push(food_sprite(
            "gui/sprites/hud/food_empty",
            food_icon,
            0x80FFFFFF,
        ));
        if index < food_fills {
            draws.push(food_sprite(
                if food_icon.half {
                    "gui/sprites/hud/food_half"
                } else {
                    "gui/sprites/hud/food_full"
                },
                food_icon,
                u32::MAX,
            ));
        }

        let saturation_icon = HungerIcon {
            x: saturation_at.0 as i32 + x,
            y: saturation_at.1 as i32,
            half: false,
        };
        draws.push(food_sprite(
            "gui/sprites/hud/food_empty",
            saturation_icon,
            0x80FFFFFF,
        ));
        if index < saturation_fills {
            let fill = saturation / 2.0 - index as f32;
            let source_x = saturation_fill_source(fill);
            draws.push(texture_draw(
                TextureDraw {
                    src: [source_x, 0, TOOLTIP_ICON_SIZE, TOOLTIP_ICON_SIZE],
                    dst: [
                        saturation_icon.x,
                        saturation_icon.y,
                        TOOLTIP_ICON_SIZE,
                        TOOLTIP_ICON_SIZE,
                    ],
                },
                u32::MAX,
            ));
        }
    }
    draws
}

fn food_properties(host: &dyn ClientHost, stack: &ItemStack) -> Option<FoodProperties> {
    let patch = stack.components().map(|components| components.as_nbt());
    if patch.is_some_and(|patch| {
        patch
            .keys()
            .any(|key| key.to_string_lossy() == "!minecraft:food")
    }) {
        return None;
    }

    let component = patch
        .and_then(|patch| {
            patch.iter().find_map(|(key, value)| {
                (key.to_string_lossy() == "minecraft:food").then_some(value)
            })
        })
        .cloned()
        .or_else(|| host.item_default_component(stack.item(), "minecraft:food"))?;
    let nutrition = component.get("nutrition").and_then(tag_number)?.max(0.0) as u32;
    let saturation = component.get("saturation").and_then(tag_number)?;
    if !saturation.is_finite() {
        return None;
    }

    Some(FoodProperties {
        nutrition,
        saturation,
        can_always_eat: component.get("can_always_eat").is_some_and(tag_bool),
    })
}

fn tag_number(tag: &Tag) -> Option<f32> {
    match tag {
        Tag::Byte(value) => Some(*value as f32),
        Tag::Short(value) => Some(*value as f32),
        Tag::Int(value) => Some(*value as f32),
        Tag::Long(value) => Some(*value as f32),
        Tag::Float(bits) => Some(f32::from_bits(*bits)),
        Tag::Double(bits) => Some(f64::from_bits(*bits) as f32),
        _ => None,
    }
}

fn tag_bool(tag: &Tag) -> bool {
    matches!(tag, Tag::Byte(value) if *value != 0)
}

fn preview_food(
    current_food: i32,
    current_saturation: f32,
    properties: FoodProperties,
) -> FoodPreview {
    let food_after = current_food
        .saturating_add(properties.nutrition.min(i32::MAX as u32) as i32)
        .clamp(0, 20);
    let saturation_after =
        (current_saturation + properties.saturation).clamp(0.0, food_after as f32);

    FoodPreview {
        food: food_after - current_food,
        saturation_gained: (saturation_after - current_saturation).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn food_bar_origin_matches_vanilla_hud_position() {
        assert_eq!(food_bar_origin(854.0, 480.0), (518, 441));
    }

    #[test]
    fn saturation_texture_uses_its_packed_atlas_id() {
        assert_eq!(SATURATION_TEXTURE, "appleskin:gui/sprites/icons");
        assert!(
            std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/appleskin/textures/gui/sprites/icons.png"
            ))
            .is_file()
        );
    }

    #[test]
    fn saturation_overlay_uses_appleskin_fill_states_and_icon_spacing() {
        assert_eq!(
            saturation_overlay(5.0, 0.0, 518, 441),
            vec![
                TextureDraw {
                    src: [27, 0, 9, 9],
                    dst: [509, 441, 9, 9],
                },
                TextureDraw {
                    src: [27, 0, 9, 9],
                    dst: [501, 441, 9, 9],
                },
                TextureDraw {
                    src: [9, 0, 9, 9],
                    dst: [493, 441, 9, 9],
                },
            ]
        );
    }

    #[test]
    fn hunger_preview_uses_vanilla_gui_sprite_paths() {
        let icon = HungerIcon {
            x: 509,
            y: 441,
            half: false,
        };
        let draw = food_sprite("gui/sprites/hud/food_full", icon, u32::MAX);
        assert_eq!(draw.sprite, "gui/sprites/hud/food_full");
    }

    #[test]
    fn tooltip_food_and_saturation_rows_use_the_component_values() {
        let draws = tooltip_value_icons(5, 7.6, (100.0, 200.0), (100.0, 212.0));
        let food_fills: Vec<_> = draws
            .iter()
            .filter(|draw| {
                draw.sprite == "gui/sprites/hud/food_full"
                    || draw.sprite == "gui/sprites/hud/food_half"
            })
            .collect();
        let saturation_fills: Vec<_> = draws
            .iter()
            .filter(|draw| draw.sprite == SATURATION_TEXTURE)
            .collect();

        assert_eq!(
            food_fills
                .iter()
                .map(|draw| draw.sprite)
                .collect::<Vec<_>>(),
            [
                "gui/sprites/hud/food_full",
                "gui/sprites/hud/food_full",
                "gui/sprites/hud/food_half"
            ]
        );
        assert_eq!(
            saturation_fills
                .iter()
                .map(|draw| (draw.src[0], draw.dst[0], draw.dst[1]))
                .collect::<Vec<_>>(),
            [
                (27.0, 100.0, 212.0),
                (27.0, 108.0, 212.0),
                (27.0, 116.0, 212.0),
                (18.0, 124.0, 212.0)
            ]
        );
        assert!(
            saturation_fills
                .iter()
                .all(|draw| draw.missing_color.is_some())
        );
    }

    #[test]
    fn tooltip_uses_bread_component_saturation_amount_directly() {
        let bread = FoodProperties {
            nutrition: 5,
            saturation: 6.0,
            can_always_eat: false,
        };
        let draws = tooltip_value_icons(
            bread.nutrition,
            bread.saturation,
            (100.0, 200.0),
            (100.0, 212.0),
        );
        let saturation_fills: Vec<_> = draws
            .iter()
            .filter(|draw| draw.sprite == SATURATION_TEXTURE)
            .map(|draw| (draw.src[0], draw.dst[0]))
            .collect();

        assert_eq!(
            saturation_fills,
            [(27.0, 100.0), (27.0, 108.0), (27.0, 116.0)]
        );
    }

    #[test]
    fn tooltip_rows_reserve_space_without_numeric_labels() {
        assert!(FOOD_TOOLTIP_ROW.chars().all(char::is_whitespace));
        assert!(SATURATION_TOOLTIP_ROW.chars().all(char::is_whitespace));
        assert!(SATURATION_TOOLTIP_ROW.len() > FOOD_TOOLTIP_ROW.len());
        assert!(FOOD_TOOLTIP_ROW.len() * 4 >= 81);
    }

    #[test]
    fn tooltip_icon_positions_match_horizons_tooltip_positioner() {
        assert_eq!(
            tooltip_position((100.0, 100.0), (320.0, 240.0), (100.0, 40.0)),
            (112.0, 88.0)
        );
        assert_eq!(
            tooltip_position((300.0, 100.0), (320.0, 240.0), (120.0, 40.0)),
            (164.0, 88.0)
        );
        assert_eq!(
            tooltip_position((100.0, 230.0), (320.0, 240.0), (100.0, 60.0)),
            (112.0, 177.0)
        );
    }

    #[test]
    fn gained_saturation_starts_at_the_current_bar_tail() {
        assert_eq!(
            saturation_overlay(5.0, 2.0, 518, 441),
            vec![
                TextureDraw {
                    src: [27, 0, 9, 9],
                    dst: [493, 441, 9, 9],
                },
                TextureDraw {
                    src: [9, 0, 9, 9],
                    dst: [485, 441, 9, 9],
                },
            ]
        );
    }

    #[test]
    fn exhaustion_progress_is_clamped_to_the_food_bar_width() {
        assert_eq!(
            exhaustion_overlay(2.0, 518, 441),
            Some(TextureDraw {
                src: [41, 18, 40, 1],
                dst: [478, 449, 40, 1],
            })
        );
        assert_eq!(exhaustion_overlay(8.0, 518, 441).unwrap().dst[2], 81);
        assert_eq!(exhaustion_overlay(0.0, 518, 441), None);
    }

    #[test]
    fn preview_caps_food_gain_at_twenty() {
        let preview = preview_food(
            18,
            2.0,
            FoodProperties {
                nutrition: 6,
                saturation: 9.6,
                can_always_eat: false,
            },
        );

        assert_eq!(preview.food, 2);
        assert!((preview.saturation_gained - 9.6).abs() < 0.001);
    }

    #[test]
    fn preview_caps_saturation_at_the_resulting_food_level() {
        let preview = preview_food(
            1,
            1.0,
            FoodProperties {
                nutrition: 4,
                saturation: 8.0,
                can_always_eat: false,
            },
        );

        assert_eq!(preview.food, 4);
        assert!((preview.saturation_gained - 4.0).abs() < 0.001);
    }

    #[test]
    fn common_food_previews_use_absolute_component_saturation_values() {
        for (nutrition, saturation, current_food, expected) in
            [(4, 2.4, 6, 2.4), (5, 6.0, 6, 6.0), (8, 12.8, 8, 12.8)]
        {
            let preview = preview_food(
                current_food,
                0.0,
                FoodProperties {
                    nutrition,
                    saturation,
                    can_always_eat: false,
                },
            );
            assert!((preview.saturation_gained - expected).abs() < 0.001);
        }
    }

    #[test]
    fn golden_apple_preview_is_one_eating_action_and_caps_at_food_level() {
        let golden_apple = FoodProperties {
            nutrition: 4,
            saturation: 9.6,
            can_always_eat: true,
        };

        let below_cap = preview_food(10, 8.0, golden_apple);
        assert_eq!(below_cap.food, 4);
        assert!((below_cap.saturation_gained - 6.0).abs() < 0.001);
        assert_eq!(
            preview_saturation_overlay(8.0, below_cap, 518, 441)
                .iter()
                .map(|draw| (draw.src[0], draw.dst[0]))
                .collect::<Vec<_>>(),
            [(27, 477), (27, 469), (27, 461)]
        );

        let full_hunger = preview_food(20, 0.0, golden_apple);
        assert_eq!(full_hunger.food, 0);
        assert!((full_hunger.saturation_gained - 9.6).abs() < 0.001);
    }
}
