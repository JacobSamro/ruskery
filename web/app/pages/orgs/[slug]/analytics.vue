<script setup lang="ts">
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const route = useRoute();
const api = useApi();
const slug = computed(() => route.params.slug as string);

interface Overview {
  pushes: number;
  pulls: number;
  blob_serves: number;
  bytes_pushed: number;
  bytes_served: number;
  storage_bytes: number;
  storage_blobs: number;
}
interface DayPoint {
  day: string;
  pushes: number;
  pulls: number;
}
interface StoragePoint {
  day: string;
  bytes: number;
}
interface RepoStat {
  repo: string;
  pushes: number;
  pulls: number;
  bytes_served: number;
  storage_bytes: number;
}
interface UserStat {
  user_id: string;
  username: string;
  pushes: number;
  pulls: number;
}
interface Analytics {
  range_days: number;
  overview: Overview;
  series: DayPoint[];
  storage: StoragePoint[];
  top_repos: RepoStat[];
  top_users: UserStat[];
}

const range = ref("30");
const data = ref<Analytics | null>(null);
const loading = ref(true);

function bytes(n: number): string {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`;
}

async function load() {
  loading.value = true;
  try {
    data.value = await api.get<Analytics>(
      `/api/v1/orgs/${slug.value}/analytics?range=${range.value}d`,
    );
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch([range, slug], load);

const labels = computed(() => data.value?.series.map((p) => p.day) ?? []);
const activitySeries = computed(() => [
  { name: "Pulls", color: "var(--primary)", values: data.value?.series.map((p) => p.pulls) ?? [] },
  { name: "Pushes", color: "#60a5fa", values: data.value?.series.map((p) => p.pushes) ?? [] },
]);
const storageSeries = computed(() => [
  { name: "Storage", color: "#34d399", values: data.value?.storage.map((p) => p.bytes) ?? [] },
]);
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Analytics</h1>
        <p class="text-sm text-muted-foreground">
          Push, pull, storage and activity for this organization.
        </p>
      </div>
      <Select v-model="range">
        <SelectTrigger class="w-36" data-testid="range"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="7">Last 7 days</SelectItem>
          <SelectItem value="30">Last 30 days</SelectItem>
          <SelectItem value="90">Last 90 days</SelectItem>
        </SelectContent>
      </Select>
    </div>

    <div v-if="loading" class="py-12 text-center text-sm text-muted-foreground">Loading…</div>

    <div v-else-if="data" class="flex flex-col gap-6">
      <!-- overview cards -->
      <div class="grid grid-cols-2 gap-4 md:grid-cols-4">
        <UiCard data-testid="metric-pulls">
          <div class="text-xs uppercase tracking-wide text-muted-foreground">Pulls</div>
          <div class="mt-1 text-2xl font-semibold">{{ data.overview.pulls.toLocaleString() }}</div>
          <div class="text-xs text-muted-foreground">last {{ data.range_days }}d</div>
        </UiCard>
        <UiCard data-testid="metric-pushes">
          <div class="text-xs uppercase tracking-wide text-muted-foreground">Pushes</div>
          <div class="mt-1 text-2xl font-semibold">{{ data.overview.pushes.toLocaleString() }}</div>
          <div class="text-xs text-muted-foreground">last {{ data.range_days }}d</div>
        </UiCard>
        <UiCard data-testid="metric-storage">
          <div class="text-xs uppercase tracking-wide text-muted-foreground">Storage</div>
          <div class="mt-1 text-2xl font-semibold">{{ bytes(data.overview.storage_bytes) }}</div>
          <div class="text-xs text-muted-foreground">{{ data.overview.storage_blobs.toLocaleString() }} blobs (dedup'd)</div>
        </UiCard>
        <UiCard>
          <div class="text-xs uppercase tracking-wide text-muted-foreground">Egress (attributed)</div>
          <div class="mt-1 text-2xl font-semibold">{{ bytes(data.overview.bytes_served) }}</div>
          <div class="text-xs text-muted-foreground">{{ data.overview.blob_serves.toLocaleString() }} layer serves</div>
        </UiCard>
      </div>

      <!-- activity -->
      <UiCard title="Push & pull activity" description="Daily manifest pushes and pulls.">
        <UiChart :series="activitySeries" :labels="labels" />
      </UiCard>

      <!-- storage growth -->
      <UiCard title="Storage growth" description="Deduplicated org storage, snapshotted daily.">
        <UiChart :series="storageSeries" :labels="labels" :format="bytes" />
      </UiCard>

      <!-- top repos -->
      <UiCard title="Top repositories" description="Most-pulled repositories in this range.">
        <div v-if="!data.top_repos.length" class="py-4 text-center text-sm text-muted-foreground">No activity yet.</div>
        <table v-else class="w-full text-sm">
          <thead class="text-left text-xs uppercase tracking-wide text-muted-foreground">
            <tr class="border-b border-border">
              <th class="px-3 py-2 font-medium">Repository</th>
              <th class="px-3 py-2 font-medium text-right">Pulls</th>
              <th class="px-3 py-2 font-medium text-right">Pushes</th>
              <th class="px-3 py-2 font-medium text-right">Size</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in data.top_repos" :key="r.repo" class="border-b border-border last:border-0">
              <td class="px-3 py-2 font-mono">{{ r.repo }}</td>
              <td class="px-3 py-2 text-right">{{ r.pulls.toLocaleString() }}</td>
              <td class="px-3 py-2 text-right">{{ r.pushes.toLocaleString() }}</td>
              <td class="px-3 py-2 text-right text-muted-foreground">{{ bytes(r.storage_bytes) }}</td>
            </tr>
          </tbody>
        </table>
      </UiCard>

      <!-- top users -->
      <UiCard title="Most active users" description="Pushes and pulls by member.">
        <div v-if="!data.top_users.length" class="py-4 text-center text-sm text-muted-foreground">No activity yet.</div>
        <table v-else class="w-full text-sm">
          <thead class="text-left text-xs uppercase tracking-wide text-muted-foreground">
            <tr class="border-b border-border">
              <th class="px-3 py-2 font-medium">User</th>
              <th class="px-3 py-2 font-medium text-right">Pushes</th>
              <th class="px-3 py-2 font-medium text-right">Pulls</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="u in data.top_users" :key="u.user_id" class="border-b border-border last:border-0">
              <td class="px-3 py-2 font-medium">{{ u.username }}</td>
              <td class="px-3 py-2 text-right">{{ u.pushes.toLocaleString() }}</td>
              <td class="px-3 py-2 text-right">{{ u.pulls.toLocaleString() }}</td>
            </tr>
          </tbody>
        </table>
      </UiCard>
    </div>
  </div>
</template>
