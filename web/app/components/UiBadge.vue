<script setup lang="ts">
// `UiBadge` — backed by the official shadcn-vue Badge. Maps the project's
// variant names onto shadcn's: default (neutral) → secondary, primary
// (accent) → default, outline → outline.
import { computed } from "vue";
import { Badge } from "@/components/ui/badge";

const props = withDefaults(
  defineProps<{ variant?: "default" | "outline" | "primary" }>(),
  { variant: "default" },
);

const mapped = computed(
  () =>
    (({ default: "secondary", primary: "default", outline: "outline" }) as const)[
      props.variant
    ],
);
</script>

<template>
  <Badge :variant="mapped">
    <slot />
  </Badge>
</template>
