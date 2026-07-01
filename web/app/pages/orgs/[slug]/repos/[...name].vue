<script setup lang="ts">
const route = useRoute();
const api = useApi();
const me = useMe();
const { confirm } = useConfirm();

const slug = computed(() => route.params.slug as string);
const name = computed(() => {
  const n = route.params.name;
  return Array.isArray(n) ? n.join("/") : (n as string);
});

interface TagDetail {
  tag: string;
  digest: string;
  size: number;
  updated_at: string;
  pull_count: number;
}
const repo = ref<{ tags: TagDetail[]; pull_prefix: string } | null>(null);
const loading = ref(true);
const copied = ref(""); // key of the last-copied affordance

const canAdmin = computed(() => {
  const role = me.value?.orgs.find((o) => o.slug === slug.value)?.role;
  return role === "owner" || role === "admin";
});

async function load() {
  loading.value = true;
  try {
    repo.value = await api.get(`/api/v1/orgs/${slug.value}/repos/${name.value}`);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

// ── group tags by digest: one image = one manifest, many tags ──
interface ImageVersion {
  digest: string;
  tags: string[];
  size: number;
  updated_at: string;
  pull_count: number;
}
const images = computed<ImageVersion[]>(() => {
  const byDigest = new Map<string, ImageVersion>();
  for (const t of repo.value?.tags ?? []) {
    let g = byDigest.get(t.digest);
    if (!g) {
      g = {
        digest: t.digest,
        tags: [],
        size: t.size,
        updated_at: t.updated_at,
        pull_count: t.pull_count,
      };
      byDigest.set(t.digest, g);
    }
    g.tags.push(t.tag);
    if (t.updated_at > g.updated_at) g.updated_at = t.updated_at;
  }
  const groups = [...byDigest.values()];
  for (const g of groups) {
    g.tags.sort((a, b) => {
      if (a === "latest") return -1;
      if (b === "latest") return 1;
      return b.localeCompare(a, undefined, { numeric: true });
    });
  }
  groups.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  return groups;
});

// The tag we suggest for a `docker pull` of a given image (prefer `latest`).
function primaryTag(img: ImageVersion): string {
  return img.tags.includes("latest") ? "latest" : img.tags[0];
}

async function copyText(text: string, key: string) {
  try {
    await navigator.clipboard?.writeText(text);
    copied.value = key;
    setTimeout(() => {
      if (copied.value === key) copied.value = "";
    }, 1500);
  } catch {
    /* clipboard unavailable */
  }
}

const router = useRouter();
async function remove() {
  if (
    !(await confirm({
      title: "Delete repository",
      message: `Delete repository "${name.value}" and all its tags? This cannot be undone.`,
      confirmText: "Delete",
      destructive: true,
    }))
  )
    return;
  await api.del(`/api/v1/orgs/${slug.value}/repos/${name.value}`);
  router.push(`/orgs/${slug.value}`);
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-center justify-between">
      <div>
        <NuxtLink :to="`/orgs/${slug}`" class="text-sm text-muted-foreground hover:text-foreground">
          ← Repositories
        </NuxtLink>
        <h1 class="mt-1 flex items-center gap-2 text-2xl font-semibold tracking-tight">
          <UiIcon name="boxes" :size="22" /> {{ name }}
        </h1>
      </div>
      <UiButton v-if="canAdmin" variant="destructive" size="sm" @click="remove">
        <UiIcon name="trash" :size="14" /> Delete
      </UiButton>
    </div>

    <UiCard v-if="repo" title="Pull this image">
      <button
        type="button"
        class="flex w-full items-center justify-between gap-3 rounded-md border border-border bg-muted/40 px-3 py-2 text-left font-mono text-sm transition-colors hover:bg-muted"
        @click="copyText(`${repo.pull_prefix}:latest`, 'pullcmd')"
      >
        <span class="truncate">{{ repo.pull_prefix }}:&lt;tag&gt;</span>
        <UiIcon
          :name="copied === 'pullcmd' ? 'check' : 'copy'"
          :size="15"
          class="shrink-0 text-muted-foreground"
        />
      </button>
    </UiCard>

    <div
      class="mt-6 overflow-hidden rounded-xl border border-border bg-card text-card-foreground shadow-sm"
    >
      <div class="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 class="text-sm font-semibold">Image versions</h2>
        <span v-if="images.length" class="text-xs text-muted-foreground">
          {{ images.length }} {{ images.length === 1 ? "image" : "images" }}
        </span>
      </div>

      <div v-if="loading" class="px-4 py-10 text-center text-sm text-muted-foreground">Loading…</div>
      <div v-else-if="!images.length" class="flex flex-col items-center gap-2 px-4 py-12 text-center">
        <UiIcon name="package" :size="28" class="text-muted-foreground" />
        <p class="text-sm text-muted-foreground">No image versions yet — push your first tag.</p>
      </div>

      <ul v-else class="divide-y divide-border">
        <li v-for="img in images" :key="img.digest" class="px-4 py-4 transition-colors hover:bg-muted/30">
          <div class="flex items-start justify-between gap-4">
            <!-- tag pills -->
            <div class="flex min-w-0 flex-wrap items-center gap-1.5">
              <span
                v-for="t in img.tags"
                :key="t"
                :class="[
                  'inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium',
                  t === 'latest'
                    ? 'border-primary/60 text-primary'
                    : 'border-border text-foreground/80',
                ]"
              >
                {{ t }}
              </span>
            </div>
            <!-- pull count -->
            <div
              class="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground"
              :title="`${img.pull_count} pulls`"
            >
              <UiIcon name="download" :size="15" />
              <span class="tabular-nums">{{ img.pull_count }}</span>
            </div>
          </div>

          <!-- meta line -->
          <div class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
            <span>Published {{ timeAgo(img.updated_at) }}</span>
            <span class="text-border">·</span>
            <span>{{ formatBytes(img.size) }}</span>
            <span class="text-border">·</span>
            <span>{{ img.tags.length }} {{ img.tags.length === 1 ? "tag" : "tags" }}</span>
            <span class="text-border">·</span>
            <button
              type="button"
              class="inline-flex items-center gap-1 hover:text-foreground"
              @click="copyText(`${repo?.pull_prefix}:${primaryTag(img)}`, `cmd:${img.digest}`)"
            >
              <UiIcon :name="copied === `cmd:${img.digest}` ? 'check' : 'copy'" :size="13" />
              {{ copied === `cmd:${img.digest}` ? "Copied" : "Copy pull command" }}
            </button>
          </div>

          <!-- digest chip (click to copy) -->
          <button
            type="button"
            class="group mt-2 flex w-full items-center gap-2 rounded-md bg-muted/60 px-3 py-1.5 text-left font-mono text-xs text-muted-foreground transition-colors hover:bg-muted"
            :title="img.digest"
            @click="copyText(img.digest, `digest:${img.digest}`)"
          >
            <span class="truncate">{{ img.digest }}</span>
            <UiIcon
              :name="copied === `digest:${img.digest}` ? 'check' : 'copy'"
              :size="13"
              class="ml-auto shrink-0 opacity-60 group-hover:opacity-100"
            />
          </button>
        </li>
      </ul>
    </div>
  </div>
</template>
