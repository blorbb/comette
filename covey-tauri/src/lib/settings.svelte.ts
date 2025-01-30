import { invoke } from "@tauri-apps/api/core";

import type { GlobalConfig, PluginConfig, PluginManifest } from "./bindings";
import type { DeepReadonly } from "./utils";

type PluginList = { name: string; config: PluginConfig }[];

export class Settings {
  // definitely assigned in constructor so will not be undefined
  public globalConfig: GlobalConfig = $state() as GlobalConfig;

  private constructor(config: GlobalConfig) {
    this.globalConfig = config;
  }

  public static async new(): Promise<Settings> {
    const config = await invoke<GlobalConfig>("get_global_config");
    console.debug("received settings", config);
    const self = new Settings(config);
    return self;
  }

  public updateBackendConfig(): void {
    console.debug("updating config to new");
    void invoke("set_global_config", {
      config: this.globalConfig,
    });
  }

  public get plugins(): PluginList {
    return Object.entries(this.globalConfig.plugins).map(([name, config]) => ({
      name,
      config,
    }));
  }

  public set plugins(plugins: PluginList) {
    console.debug("setting plugins", plugins);

    this.globalConfig.plugins = Object.fromEntries(
      plugins.map(({ name, config }) => [name, config]),
    );
  }

  public getPlugin(pluginName: string): PluginConfig {
    return this.globalConfig.plugins[pluginName];
  }

  public async fetchManifestOf(
    pluginName: string,
  ): Promise<DeepReadonly<PluginManifest>> {
    return invoke("get_manifest", { pluginName });
  }
}
