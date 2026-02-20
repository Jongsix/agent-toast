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
let statusInterval: ReturnType<typeof setInterval> | null = null;

async function pollStatus() {
  try {
    tunnelStatus.value = await invoke<string>("get_tunnel_status");
  } catch {
    tunnelStatus.value = "Error";
  }
}

onMounted(() => {
  if (config.value.remoteEnabled) {
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
  () => config.value.remoteEnabled,
  (enabled) => {
    if (enabled && !statusInterval) {
      statusInterval = setInterval(pollStatus, 2000);
      pollStatus();
    } else if (!enabled && statusInterval) {
      clearInterval(statusInterval);
      statusInterval = null;
      tunnelStatus.value = "Disconnected";
    }
  },
);

async function connectTunnel() {
  try {
    await invoke("connect_ssh_tunnel");
    await pollStatus();
  } catch (e) {
    toast.error(String(e));
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
    config.value = { ...config.value, remoteToken: token };
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
  const port = config.value.sshRemotePort || 19876;
  const token = config.value.remoteToken || "YOUR_TOKEN";
  return `curl -s -X POST \\\n  -H "X-Agent-Toast-Token: ${token}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"pid":0,"event":"task_complete","message":"Build done","source":"remote"}' \\\n  http://localhost:${port}/notify`;
});

const hookConfigExample = computed(() => {
  const port = config.value.sshRemotePort || 19876;
  const token = config.value.remoteToken || "YOUR_TOKEN";
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
}`;
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

const statusLabel = computed(() => {
  switch (tunnelStatus.value) {
    case "Connected":
      return t("remote.connected");
    case "Connecting":
      return t("remote.connecting");
    case "Disconnected":
      return t("remote.disconnected");
    default:
      return tunnelStatus.value.startsWith("Error:")
        ? tunnelStatus.value
        : t("remote.error");
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
        <Switch v-model="config.remoteEnabled" />
      </div>

      <template v-if="config.remoteEnabled">
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
              v-model="config.remotePort"
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
                :model-value="config.remoteToken"
                readonly
                class="flex-1 h-7 text-xs font-mono"
                :placeholder="t('remote.authToken')"
              />
              <Button
                variant="outline"
                size="sm"
                class="h-7 px-2 text-xs"
                @click="copyToClipboard(config.remoteToken)"
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
              v-model="config.sshHost"
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
              v-model="config.sshPort"
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
              v-model="config.sshUser"
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
              v-model="config.sshKeyPath"
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
              v-model="config.sshRemotePort"
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
            <Switch v-model="config.sshAutoConnect" />
          </div>
        </div>

        <!-- Connection Control Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.connectionSection") }}</span
          >

          <div
            class="flex items-center justify-between bg-card border rounded-[10px] px-3.5 py-3"
          >
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
                :disabled="isConnecting || !config.sshHost"
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
        </div>

        <!-- Setup Guide Section -->
        <div class="flex flex-col gap-2">
          <span
            class="text-xs font-semibold text-muted-foreground uppercase tracking-wide px-0.5"
            >{{ t("remote.guideSection") }}</span
          >

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
