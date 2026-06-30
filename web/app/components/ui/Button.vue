<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    variant?: "default" | "outline" | "ghost" | "destructive";
    size?: "default" | "sm" | "icon";
    type?: "button" | "submit";
    disabled?: boolean;
  }>(),
  { variant: "default", size: "default", type: "button", disabled: false },
);

const base =
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-[var(--radius)] text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-primary)] disabled:opacity-50 disabled:pointer-events-none";
const variants: Record<string, string> = {
  default: "bg-[var(--color-primary)] text-[var(--color-primary-fg)] hover:opacity-90",
  outline: "border border-[var(--color-border)] bg-transparent hover:bg-[var(--color-surface)]",
  ghost: "hover:bg-[var(--color-surface)]",
  destructive: "bg-red-600 text-white hover:bg-red-500",
};
const sizes: Record<string, string> = {
  default: "h-9 px-4 py-2",
  sm: "h-8 px-3 text-xs",
  icon: "h-9 w-9",
};
const cls = computed(() => [base, variants[props.variant], sizes[props.size]].join(" "));
</script>

<template>
  <button :type="type" :disabled="disabled" :class="cls">
    <slot />
  </button>
</template>
