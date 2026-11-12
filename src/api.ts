import { invoke } from '@tauri-apps/api/core';
import { z } from 'zod';

export const trendSchema = z.enum(['hours', '7d', '30d', 'week', 'month']);
export type Trend = z.infer<typeof trendSchema>;
const bytesSchema = z.string().regex(/^\d+$/);
export const trafficRowSchema = z.object({
  key: z.string(), label: z.string(), day: z.string().optional(), hour: z.number().int().optional(),
  uploaded: bytesSchema, downloaded: bytesSchema, samples: z.number(), coveredSeconds: z.number(),
  gapSeconds: z.number(), legacyCoverage: z.boolean(), future: z.boolean().optional(),
});
export type TrafficRow = z.infer<typeof trafficRowSchema>;
export const rankRowSchema = z.object({ name: z.string(), uploaded: bytesSchema, downloaded: bytesSchema });
export type RankRow = z.infer<typeof rankRowSchema>;
export const dashboardRequestSchema = z.object({
  day: z.string().regex(/^\d{4}-\d{2}-\d{2}$/), trend: trendSchema,
  hour: z.number().int().min(0).max(23), minute: z.number().int().min(0).max(55).nullable(),
});
export type DashboardRequest = z.infer<typeof dashboardRequestSchema>;
export const dashboardSchema = z.object({
  revision: z.number(), sampledAt: z.number().nullable(),
  status: z.object({ connected: z.boolean(), message: z.string(), lastSampleAt: z.number().nullable() }),
  days: z.array(trafficRowSchema), hours: z.array(trafficRowSchema), apps: z.array(rankRowSchema),
  summary: z.object({ uploaded: bytesSchema, downloaded: bytesSchema, coveredSeconds: z.number(),
    gapSeconds: z.number(), legacyCoverage: z.boolean(), unattributedUp: bytesSchema, unattributedDown: bytesSchema }),
  selected: trafficRowSchema.nullable(),
  rangeStart: z.string(), rangeEnd: z.string(), dataDir: z.string(),
});
export type DashboardSnapshot = z.infer<typeof dashboardSchema>;
export const settingsSchema = z.object({
  alertsEnabled: z.boolean(), thresholdMib: z.number().int().min(1).max(1_000_000),
  startupEnabled: z.boolean(), startupAvailable: z.boolean(),
});
export type Settings = z.infer<typeof settingsSchema>;
export interface TargetRequest { day: string; app: string; hour: number | null; minute: number | null }
export interface MonitorApi {
  dashboard(request: DashboardRequest): Promise<DashboardSnapshot>;
  targets(request: TargetRequest): Promise<RankRow[]>;
  settings(): Promise<Settings>;
  saveSettings(settings: Settings): Promise<void>;
  diagnostics(): Promise<unknown>;
}
export const monitorApi: MonitorApi = {
  dashboard: async (request) => dashboardSchema.parse(await invoke('get_dashboard', { request })),
  targets: async (request) => z.array(rankRowSchema).parse(await invoke('get_targets', { request })),
  settings: async () => settingsSchema.parse(await invoke('get_settings')),
  saveSettings: async (settings) => { await invoke('save_settings', { settings }); },
  diagnostics: () => invoke('get_diagnostics'),
};
