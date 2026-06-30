<script setup lang="ts">
// Minimal dependency-free SVG line chart for analytics time series. Renders one
// or more series over a shared x-axis into a responsive viewBox.
interface Series {
  name: string;
  color: string;
  values: number[];
}
const props = withDefaults(
  defineProps<{
    series: Series[];
    labels?: string[];
    height?: number;
    format?: (n: number) => string;
  }>(),
  { height: 160 },
);

const W = 600;
const PAD = 10;

const max = computed(() =>
  Math.max(1, ...props.series.flatMap((s) => s.values)),
);
const n = computed(() =>
  Math.max(1, ...props.series.map((s) => s.values.length)),
);

function x(i: number) {
  if (n.value <= 1) return PAD;
  return PAD + (i * (W - PAD * 2)) / (n.value - 1);
}
function y(v: number) {
  return props.height - PAD - (v / max.value) * (props.height - PAD * 2);
}
function line(values: number[]) {
  return values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
}
function area(values: number[]) {
  if (!values.length) return "";
  const top = line(values);
  return `${PAD.toFixed(1)},${(props.height - PAD).toFixed(1)} ${top} ${x(values.length - 1).toFixed(1)},${(props.height - PAD).toFixed(1)}`;
}

const fmt = (v: number) => (props.format ? props.format(v) : String(v));
const empty = computed(() => props.series.every((s) => s.values.every((v) => v === 0)));
</script>

<template>
  <div>
    <div class="mb-2 flex flex-wrap gap-3 text-xs">
      <span v-for="s in series" :key="s.name" class="flex items-center gap-1.5 text-[var(--color-muted)]">
        <span class="inline-block h-2 w-2 rounded-full" :style="{ background: s.color }" />
        {{ s.name }}
      </span>
      <span class="ml-auto text-[var(--color-muted)]">peak {{ fmt(max) }}</span>
    </div>
    <svg
      :viewBox="`0 0 ${W} ${height}`"
      preserveAspectRatio="none"
      class="w-full"
      :style="{ height: `${height}px` }"
      role="img"
    >
      <template v-for="(s, si) in series" :key="s.name">
        <polygon
          v-if="si === 0"
          :points="area(s.values)"
          :fill="s.color"
          fill-opacity="0.08"
          stroke="none"
        />
        <polyline
          :points="line(s.values)"
          fill="none"
          :stroke="s.color"
          stroke-width="2"
          stroke-linejoin="round"
          stroke-linecap="round"
          vector-effect="non-scaling-stroke"
        />
      </template>
    </svg>
    <p v-if="empty" class="mt-1 text-center text-xs text-[var(--color-muted)]">
      No data in this range yet.
    </p>
  </div>
</template>
