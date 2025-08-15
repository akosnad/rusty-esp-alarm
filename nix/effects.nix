{ config, withSystem, ... }:
let
  inherit (withSystem "x86_64-linux" ({ config, ... }: config.project)) package;
in
{
  herculesCI = herculesCI: {
    onPush.default.outputs.effects.deploy = withSystem config.defaultEffectSystem (
      { pkgs, hci-effects, ... }:
      hci-effects.runIf (herculesCI.config.repo.branch == "main") (
        hci-effects.mkEffect {
          effectScript = ''
            ${pkgs.lib.getExe' pkgs.mosquitto "mosquitto_pub"} -L mqtt://gaia_alarm/ota -f ${package}/ota.bin -q 2
          '';
        }
      )
    );
  };
}
