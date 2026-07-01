<script setup lang="ts">
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

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
    { to: `/orgs/${s}/analytics`, label: "Analytics", icon: "grid" },
    { to: `/orgs/${s}/teams`, label: "Teams", icon: "team" },
    { to: `/orgs/${s}/members`, label: "Members", icon: "users" },
    { to: `/orgs/${s}/settings`, label: "Settings", icon: "settings" },
  ];
});

function switchOrg(slug: unknown) {
  if (slug) router.push(`/orgs/${String(slug)}`);
}

const { confirm } = useConfirm();

async function logout() {
  if (
    !(await confirm({
      title: "Sign out",
      message: "Sign out of ruskery?",
      confirmText: "Sign out",
    }))
  )
    return;
  await api.post("/api/v1/auth/logout").catch(() => {});
  me.value = null;
  await router.push("/login");
}

const linkClass =
  "flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground";
const activeClass = "bg-accent !text-accent-foreground";
</script>

<template>
  <div v-if="bare" class="relative flex min-h-full items-center justify-center p-6">
    <div class="absolute right-4 top-4">
      <ModeToggle />
    </div>
    <slot />
  </div>

  <div v-else class="flex min-h-full">
    <aside class="flex w-60 flex-col border-r border-border bg-card p-4">
      <NuxtLink to="/" class="mb-6 flex items-center gap-2 px-2">
        <div class="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <UiIcon name="grid" :size="16" />
        </div>
        <span class="text-lg font-semibold tracking-tight">ruskery</span>
      </NuxtLink>

      <Select
        v-if="me && me.orgs.length"
        :model-value="currentSlug"
        @update:model-value="switchOrg"
      >
        <SelectTrigger class="mb-4 w-full" data-testid="org-switcher"><SelectValue placeholder="Select org" /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="o in me.orgs" :key="o.slug" :value="o.slug">{{ o.name }}</SelectItem>
        </SelectContent>
      </Select>

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

        <div class="my-2 border-t border-border" />

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

        <NuxtLink
          v-if="me?.user.is_admin"
          to="/settings"
          :class="linkClass"
          :active-class="activeClass"
        >
          <UiIcon name="settings" :size="16" />
          Instance Settings
        </NuxtLink>

        <NuxtLink to="/tokens" :class="linkClass" :active-class="activeClass">
          <UiIcon name="key" :size="16" />
          Access Tokens
        </NuxtLink>
      </nav>

      <div class="mt-4 border-t border-border pt-4">
        <div class="mb-2 px-2 text-sm">
          <div class="font-medium">{{ me?.user.username }}</div>
          <div class="truncate text-xs text-muted-foreground">{{ me?.user.email }}</div>
        </div>
        <div class="flex items-center gap-2">
          <UiButton variant="ghost" size="sm" class="flex-1 !justify-start" data-testid="sign-out" @click="logout">
            <UiIcon name="logout" :size="16" /> Sign out
          </UiButton>
          <ModeToggle />
        </div>
      </div>
    </aside>

    <main class="flex-1 overflow-auto">
      <div class="mx-auto max-w-5xl p-8">
        <slot />
      </div>
    </main>
  </div>
</template>
