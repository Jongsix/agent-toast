<script setup lang="ts">
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  NumberField,
  NumberFieldContent,
  NumberFieldDecrement,
  NumberFieldIncrement,
  NumberFieldInput,
} from "@/components/ui/number-field";
import { Switch } from "@/components/ui/switch";
import { invoke } from "@tauri-apps/api/core";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { toast } from "vue-sonner";
import type { HookConfig } from "../types";

const config = defineModel<HookConfig>({ required: true });
const { t } = useI18n();

const tunnelStatus = ref("Disconnected");
const sshLogPath = ref("");
let statusInterval: ReturnType<typeof setInterval> | null = null;

async function pollStatus() {
  try {
    tunnelStatus.value = await invoke<string>("get_tunnel_status");
  } catch {
    tunnelStatus.value = "Error";
  }
}

onMounted(async () => {
  sshLogPath.value = await invoke<string>("get_ssh_log_path");
  if (config.value.remote_enabled) {
    pollStatus();
    statusInterval = setInterval(pollStatus, 2000);
  }
});

onUnmounted(() => {
  if (statusInterval) {
    clearInterval(statusInterval);
    statusInterval = null;
  }
});

watch(
  () => config.value.remote_enabled,
  (enabled) => {
    if (enabled && !statusInterval) {
      statusInterval = setInterval(pollStatus, 2000);
      pollStatus();
    } else if (!enabled && statusInterval) {
      clearInterval(statusInterval);
      statusInterval = null;
      tunnelStatus.value = "Disconnected";
    }
    // Persist the enabled/disabled state immediately
    saveRemoteSettings().catch(() => {});
  },
);

async function saveRemoteSettings() {
  await invoke("save_remote_config", {
    remote_enabled: config.value.remote_enabled,
    remote_port: config.value.remote_port,
    remote_token: config.value.remote_token,
    ssh_host: config.value.ssh_host,
    ssh_port: config.value.ssh_port,
    ssh_user: config.value.ssh_user,
    ssh_key_path: config.value.ssh_key_path,
    ssh_remote_port: config.value.ssh_remote_port,
    ssh_auto_connect: config.value.ssh_auto_connect,
  });
}

async function connectTunnel() {
  try {
    // Persist remote settings before connecting so they survive app restart
    await saveRemoteSettings();
    await invoke("connect_ssh_tunnel", {
      ssh_host: config.value.ssh_host,
      ssh_port: config.value.ssh_port,
      ssh_user: config.value.ssh_user,
      ssh_key_path: config.value.ssh_key_path,
      ssh_remote_port: config.value.ssh_remote_port,
      remote_port: config.value.remote_port,
      remote_token: config.value.remote_token,
    });
    await pollStatus();
  } catch (e) {
    const msg = String(e);
    toast.error(msg, { duration: 8000 });
    await pollStatus();
  }
}

async function disconnectTunnel() {
  try {
    await invoke("disconnect_ssh_tunnel");
    await pollStatus();
  } catch (e) {
    toast.error(String(e));
  }
}

async function testConnection() {
  try {
    await invoke<string>("test_remote_connection");
    toast.success(t("remote.testSuccess"));
  } catch (e) {
    toast.error(`${t("remote.testFailed")}: ${String(e)}`);
  }
}

async function regenerateToken() {
  try {
    const token = await invoke<string>("generate_remote_token");
    config.value = { ...config.value, remote_token: token };
  } catch (e) {
    toast.error(String(e));
  }
}

async function copyToClipboard(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast.success(t("remote.copied"));
  } catch {
    toast.error("Failed to copy");
  }
}

const curlCommand = computed(() => {
  const port = config.value.ssh_remote_port || 19876;
  const token = config.value.remote_token || "YOUR_TOKEN";
  return `curl -s -X POST \\\n  -H "X-Agent-Toast-Token: ${token}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"pid":0,"event":"task_complete","message":"Build done","source":"remote"}' \\\n  http://localhost:${port}/notify`;
});

const hookConfigExample = computed(() => {
  const port = config.value.ssh_remote_port || 19876;
  const token = config.value.remote_token || "YOUR_TOKEN";
  return `{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "curl -s -X POST -H 'X-Agent-Toast-Token: ${token}' -H 'Content-Type: application/json' -d '{\\"pid\\":0,\\"event\\":\\"task_complete\\",\\"message\\":\\"Task done\\",\\"source\\":\\"remote\\"}' http://localhost:${port}/notify"
      }]
    }]
  }
}

Linux/macOS:
curl -s -X POST -H 'X-Agent-Toast-Token: ${token}' -H 'Content-Type: application/json' -d '{"pid":0,"event":"task_complete","message":"Task done","source":"remote"}' http://localhost:${port}/notify`;
});

const isConnected = computed(() => tunnelStatus.value === "Connected");
const isConnecting = computed(() => tunnelStatus.value === "Connecting");

const statusBadgeVariant = computed(() => {
  switch (tunnelStatus.value) {
    case "Connected":
      return "default" as const;
    case "Connecting":
      return "secondary" as const;
    case "Disconnected":
      return "outline" as const;
    default:
      return "destructive" as const;
  }
});

const isError = computed(
  () =>
    tunnelStatus.value !== "Connected" &&
    tunnelStatus.value !== "Connecting" &&
    tunnelStatus.value !== "Disconnected",
);

const statusLabel = computed(() => {
  switch (tunnelStatus.value) {
    case "Connected":
      return t("remote.connected");
    case "Connecting":
      return t("remote.connecting");
    case "Disconnected":
      return t("remote.disconnected");
    default:
      return t("remote.error");
  }
});
</script>

<template>
  <div class="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto">
    <p class="text-[13px] text-muted-foreground">{{ t("remote.enableDesc") }}</p>

    <div class="flex flex-col gap-2">
      <!-- Enable Remote Notifications -->
      <div
        class="flex items-center justify-between bg-card border rounded-lg px-3.5 py-3"
      >
        <span class="text-sm font-medium text-foreground">{{
          t("remote.enable")
        }}</span>
        <Switch v-model="config.remote_enabled" />
      </div>

      <template v-if="config.remote_enabled">
        <!-- Server Configuration Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.serverSection") }}</span
          >

          <!-- Local Port -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.localPort")
            }}</span>
            <NumberField
              v-model="config.remote_port"
              :min="1024"
              :max="65535"
              :step="1"
              class="w-[110px]"
            >
              <NumberFieldContent>
                <NumberFieldDecrement class="p-2" />
                <NumberFieldInput class="h-7 text-xs" />
                <NumberFieldIncrement class="p-2" />
              </NumberFieldContent>
            </NumberField>
          </div>

          <!-- Auth Token -->
          <div
            class="flex flex-col gap-2 bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.authToken")
            }}</span>
            <div class="flex items-center gap-2">
              <Input
                :model-value="config.remote_token"
                readonly
                class="flex-1 h-7 text-xs font-mono"
                :placeholder="t('remote.authToken')"
              />
              <Button
                variant="outline"
                size="sm"
                class="h-7 px-2 text-xs"
                @click="copyToClipboard(config.remote_token)"
              >
                {{ t("remote.copy") }}
              </Button>
              <Button
                variant="outline"
                size="sm"
                class="h-7 px-2 text-xs"
                @click="regenerateToken"
              >
                {{ t("remote.regenerate") }}
              </Button>
            </div>
          </div>
        </div>

        <!-- SSH Tunnel Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.sshSection") }}</span
          >

          <!-- SSH Host -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.sshHost")
            }}</span>
            <Input
              v-model="config.ssh_host"
              class="w-[180px] h-7 text-xs"
              placeholder="user@host.example.com"
            />
          </div>

          <!-- SSH Port -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.sshPort")
            }}</span>
            <NumberField
              v-model="config.ssh_port"
              :min="1"
              :max="65535"
              :step="1"
              class="w-[110px]"
            >
              <NumberFieldContent>
                <NumberFieldDecrement class="p-2" />
                <NumberFieldInput class="h-7 text-xs" />
                <NumberFieldIncrement class="p-2" />
              </NumberFieldContent>
            </NumberField>
          </div>

          <!-- SSH Username -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.sshUser")
            }}</span>
            <Input
              v-model="config.ssh_user"
              class="w-[180px] h-7 text-xs"
              placeholder="username"
            />
          </div>

          <!-- SSH Key Path -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.sshKeyPath")
            }}</span>
            <Input
              v-model="config.ssh_key_path"
              class="w-[180px] h-7 text-xs"
              placeholder="~/.ssh/id_rsa"
            />
          </div>

          <!-- Remote Tunnel Port -->
          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.sshRemotePort")
            }}</span>
            <NumberField
              v-model="config.ssh_remote_port"
              :min="1024"
              :max="65535"
              :step="1"
              class="w-[110px]"
            >
              <NumberFieldContent>
                <NumberFieldDecrement class="p-2" />
                <NumberFieldInput class="h-7 text-xs" />
                <NumberFieldIncrement class="p-2" />
              </NumberFieldContent>
            </NumberField>
          </div>

          <!-- Auto-connect on start -->
          <div
            class="flex items-center justify-between bg-card border rounded-lg px-3.5 py-3"
          >
            <span class="text-sm font-medium text-foreground">{{
              t("remote.autoConnect")
            }}</span>
            <Switch v-model="config.ssh_auto_connect" />
          </div>
        </div>

        <!-- Connection Control Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.connectionSection") }}</span
          >

          <div
            class="flex flex-col gap-2 bg-card border rounded-[10px] px-3.5 py-3"
          >
            <div class="flex items-center justify-between">
              <div class="flex items-center gap-2">
                <Badge :variant="statusBadgeVariant" class="text-xs">{{
                  statusLabel
                }}</Badge>
              </div>
              <div class="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  class="h-7 text-xs"
                  :disabled="isConnecting"
                  @click="testConnection"
                >
                  {{ t("remote.testConnection") }}
                </Button>
                <Button
                  v-if="!isConnected"
                  size="sm"
                  class="h-7 text-xs"
                  :disabled="isConnecting || !config.ssh_host"
                  @click="connectTunnel"
                >
                  {{
                    isConnecting ? t("remote.connecting") : t("remote.connect")
                  }}
                </Button>
                <Button
                  v-else
                  variant="destructive"
                  size="sm"
                  class="h-7 text-xs"
                  @click="disconnectTunnel"
                >
                  {{ t("remote.disconnect") }}
                </Button>
              </div>
            </div>
            <!-- Error log path hint -->
            <div
              v-if="isError && sshLogPath"
              class="text-[11px] text-destructive/80 break-all"
            >
              {{ t("remote.logHint") }}
              <code class="text-[10px] text-foreground/60">{{ sshLogPath }}</code>
            </div>
          </div>
        </div>

        <!-- Setup Guide Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.guideSection") }}</span
          >

          <!-- sshd_config GatewayPorts -->
          <div
            class="flex flex-col gap-2 bg-card border rounded-[10px] px-3.5 py-3"
          >
            <span class="text-xs font-medium text-foreground">{{
              t("remote.sshdConfigTitle")
            }}</span>
            <p class="text-[11px] text-muted-foreground">{{ t("remote.sshdConfigDesc") }}</p>
            <pre
              class="text-[10px] font-mono text-muted-foreground bg-muted rounded p-2 overflow-x-auto whitespace-pre-wrap break-all"
>GatewayPorts yes</pre>
            <p class="text-[11px] text-muted-foreground">{{ t("remote.sshdConfigRestart") }}</p>
            <pre
              class="text-[10px] font-mono text-muted-foreground bg-muted rounded p-2 overflow-x-auto whitespace-pre-wrap break-all"
>sudo systemctl restart sshd</pre>
          </div>

          <!-- curl command -->
          <div
            class="flex flex-col gap-2 bg-card border rounded-[10px] px-3.5 py-3"
          >
            <div class="flex items-center justify-between">
              <span class="text-xs font-medium text-foreground">{{
                t("remote.curlCommand")
              }}</span>
              <Button
                variant="outline"
                size="sm"
                class="h-6 px-2 text-xs"
                @click="copyToClipboard(curlCommand)"
              >
                {{ t("remote.copy") }}
              </Button>
            </div>
            <pre
              class="text-[10px] font-mono text-muted-foreground bg-muted rounded p-2 overflow-x-auto whitespace-pre-wrap break-all"
              >{{ curlCommand }}</pre
            >
          </div>

          <!-- Hook config example -->
          <div
            class="flex flex-col gap-2 bg-card border rounded-[10px] px-3.5 py-3"
          >
            <div class="flex items-center justify-between">
              <span class="text-xs font-medium text-foreground">{{
                t("remote.hookConfig")
              }}</span>
              <Button
                variant="outline"
                size="sm"
                class="h-6 px-2 text-xs"
                @click="copyToClipboard(hookConfigExample)"
              >
                {{ t("remote.copy") }}
              </Button>
            </div>
            <pre
              class="text-[10px] font-mono text-muted-foreground bg-muted rounded p-2 overflow-x-auto whitespace-pre-wrap break-all"
              >{{ hookConfigExample }}</pre
            >
          </div>
        </div>
      </template>
    </div>
  </div>
</template>
