# AppleSkin for Horizon

A native Rust port of AppleSkin's food-related HUD features for Horizon.

Implemented: food and saturation item tooltips, an AppleSkin-style saturation
overlay and exhaustion strip on the vanilla food bar, held-food hunger/saturation
icon previews, an F3 food debug entry, and optional server-to-client saturation
sync, using Horizon's food-component saturation amount directly for all foods.
Accurate held-food saturation previews on multiplayer servers
require AppleSkin for Horizon on the server; clients fall back to Horizon's
vanilla-synchronized saturation when the server does not provide the payload.
A health restoration preview and exhaustion synchronization are not implemented.

This project targets Horizon build `5ef7734fdd854add5efd6817178be3c988d8d050`
(Minecraft 26.3). Package the source mod with:

```sh
cargo horizon-mod build
```

This creates `target/horizon-mod/appleskin.hmod`. Put it in an instance's
`horizon-mods/` folder, open that instance's **Mods** page in Horizon Launcher,
enable AppleSkin, then choose **Build**. The launcher compiles native mods into
the instance; this requires explicitly trusting native code, which runs outside
a sandbox.

AppleSkin's original project is available at
<https://github.com/squeek502/AppleSkin> and is released under the Unlicense.
