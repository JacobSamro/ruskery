<script setup lang="ts">
const me = useMe();
const route = useRoute();
const router = useRouter();
const api = useApi();

const bare = computed(
  () => !me.value || route.path === "/login" || route.path === "/setup",
);

const currentSlug = computed(
  () => (route.params.slug as string) || me.value?.orgs[0]?.slug || "",
);

const nav = computed(() => {
  const s = currentSlug.value;
  if (!s) return [];
  return [
    { to: `/orgs/${s}`, label: "Repositories", icon: "boxes", exact: true },
    { to: `/orgs/${s}/teams`, label: "Teams", icon: "team" },
    { to: `/orgs/${s}/members`, label: "Members", icon: "users" },
    { to: `/orgs/${s}/settings`, label: "Settings", icon: "settings" },
  ];
});

function switchOrg(e: Event) {
  const slug = (e.target as HTMLSelectElement).value;
  router.push(`/orgs/${slug}`);
}

async function logout() {
  await api.post("/api/v1/auth/logout").catch(() => {});
  me.value = null;
  await router.push("/login");
}

const linkClass =
  "flex items-center gap-3 rounded-[var(--radius)] px-3 py-2 text-sm font-medium text-[var(--color-muted)] transition-colors hover:bg-[var(--color-bg)] hover:text-[var(--color-fg)]";
const activeClass = "bg-[var(--color-bg)] !text-[var(--color-fg)]";
</script>

<template>
  <div v-if="bare" class="flex min-h-full items-center justify-center p-6">
    <slot />
  </div>

  <div v-else class="flex min-h-full">
    <aside class="flex w-60 flex-col border-r border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      <NuxtLink to="/" class="mb-6 flex items-center gap-2 px-2">
        <div class="flex h-7 w-7 items-center justify-center rounded-md bg-[var(--color-primary)] text-[var(--color-primary-fg)]">
          <UiIcon name="grid" :size="16" />
        </div>
        <span class="text-lg font-semibold tracking-tight">ruskery</span>
      </NuxtLink>

      <select
        v-if="me && me.orgs.length"
        class="mb-4 h-9 w-full rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-primary)]"
        :value="currentSlug"
        @change="switchOrg"
      >
        <option v-for="o in me.orgs" :key="o.slug" :value="o.slug">{{ o.name }}</option>
      </select>

      <nav class="flex flex-1 flex-col gap-1">
        <NuxtLink
          v-for="n in nav"
          :key="n.to"
          :to="n.to"
          :class="linkClass"
          :active-class="n.exact ? '' : activeClass"
          :exact-active-class="activeClass"
        >
          <UiIcon :name="n.icon" :size="16" />
          {{ n.label }}
        </NuxtLink>

        <div class="my-2 border-t border-[var(--color-border)]" />

        <NuxtLink
          v-if="me?.user.is_admin"
          to="/orgs"
          :class="linkClass"
          :active-class="''"
          :exact-active-class="activeClass"
        >
          <UiIcon name="grid" :size="16" />
          Organizations
        </NuxtLink>

        <NuxtLink to="/tokens" :class="linkClass" :active-class="activeClass">
          <UiIcon name="key" :size="16" />
          Access Tokens
        </NuxtLink>
      </nav>

      <div class="mt-4 border-t border-[var(--color-border)] pt-4">
        <div class="mb-2 px-2 text-sm">
          <div class="font-medium">{{ me?.user.username }}</div>
          <div class="truncate text-xs text-[var(--color-muted)]">{{ me?.user.email }}</div>
        </div>
        <UiButton variant="ghost" size="sm" class="w-full !justify-start" @click="logout">
          <UiIcon name="logout" :size="16" /> Sign out
        </UiButton>
      </div>
    </aside>

    <main class="flex-1 overflow-auto">
      <div class="mx-auto max-w-5xl p-8">
        <slot />
      </div>
    </main>
  </div>
</template>
