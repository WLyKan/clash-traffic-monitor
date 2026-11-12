import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';
import * as echarts from 'echarts/core';
import { BarChart, type BarSeriesOption } from 'echarts/charts';
import { GridComponent, type GridComponentOption } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import type { TrafficRow } from './api';
import { formatBytes, hourPeriod, qualityText, totalBytes } from './format';

echarts.use([BarChart, GridComponent, CanvasRenderer]);
type Option = echarts.ComposeOption<BarSeriesOption | GridComponentOption>;
const grid = { left: 76, right: 18, top: 20, bottom: 32 } as const;

interface Props {
  rows: TrafficRow[];
  selectedKey?: string;
  daily: boolean;
  onSelect(row: TrafficRow): void;
}

export default function TrendChart({ rows, selectedKey, daily, onSelect }: Props) {
  const surface = useRef<HTMLDivElement>(null);
  const chart = useRef<echarts.ECharts | null>(null);
  const [hoverKey, setHoverKey] = useState<string | null>(null);
  const [focusKey, setFocusKey] = useState<string | null>(null);
  const [width, setWidth] = useState(720);
  const id = useId();
  const hovered = rows.find((row) => row.key === hoverKey);
  const focusedKey = daily ? focusKey ?? selectedKey : selectedKey;
  const selectedIndex = Math.max(0, rows.findIndex((row) => row.key === focusedKey));
  const hoveredIndex = rows.findIndex((row) => row.key === hoverKey);

  useEffect(() => {
    const node = surface.current;
    if (!node) return;
    const instance = echarts.init(node, undefined, { renderer: 'canvas' });
    chart.current = instance;
    const resize = new ResizeObserver(() => {
      instance.resize();
      setWidth(node.clientWidth);
    });
    resize.observe(node);
    setWidth(node.clientWidth);
    return () => { resize.disconnect(); instance.dispose(); chart.current = null; };
  }, []);

  useEffect(() => {
    const option: Option = {
      animation: false,
      grid,
      xAxis: { type: 'category', data: rows.map((row) => row.label), axisTick: { show: false },
        axisLine: { lineStyle: { color: '#dbe3e0' } }, axisLabel: { color: '#586a65', fontSize: 11,
          interval: rows.length <= 12 ? 0 : daily ? 4 : 2 } },
      yAxis: { type: 'value', min: 0, splitNumber: 2, axisLabel: { color: '#586a65', fontSize: 11,
        formatter: (value: number) => formatBytes(BigInt(Math.round(Math.max(0, value)))) },
        splitLine: { lineStyle: { color: '#e1e7e4', type: 'dashed' } } },
      series: [
        { type: 'bar', name: '下载', stack: 'proxy', barMaxWidth: 27,
          itemStyle: { color: '#23776a' }, data: rows.map((row) => Number(row.downloaded)) },
        { type: 'bar', name: '上传', stack: 'proxy', barMaxWidth: 27,
          itemStyle: { color: '#4e78b5', borderRadius: [3, 3, 0, 0] }, data: rows.map((row) => Number(row.uploaded)) },
      ],
    };
    chart.current?.setOption(option, true);
  }, [rows, daily]);

  const navigate = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!rows.length) return;
    let index = selectedIndex;
    if (event.key === 'ArrowLeft') index--;
    else if (event.key === 'ArrowRight') index++;
    else if (event.key === 'Home') index = 0;
    else if (event.key === 'End') index = rows.length - 1;
    else if (event.key === 'Enter') { onSelect(rows[index]); event.preventDefault(); return; }
    else return;
    event.preventDefault();
    const row = rows[Math.max(0, Math.min(rows.length - 1, index))];
    if (daily) setFocusKey(row.key);
    else onSelect(row);
  };

  const tooltipWidth = Math.min(350, Math.max(100, width - 16));
  const plotWidth = Math.max(1, width - grid.left - grid.right);
  const columnCenter = grid.left + (hoveredIndex + 0.5) * plotWidth / Math.max(1, rows.length);
  const tooltipLeft = Math.max(8, Math.min(width - tooltipWidth - 8, columnCenter + 8));

  return <div className="trend-chart" role="group" aria-label={daily ? '每日代理趋势，方向键选择日期，回车查看小时' : '每小时代理趋势，方向键选择小时'}
    tabIndex={0} onKeyDown={navigate} aria-activedescendant={rows.length ? `${id}-${selectedIndex}` : undefined}>
    <div ref={surface} className="chart-surface" data-testid="chart-surface" aria-hidden="true" />
    <div className="plot-columns" data-testid="plot-columns" style={{ left: grid.left, right: grid.right, top: grid.top, bottom: grid.bottom,
      gridTemplateColumns: `repeat(${Math.max(1, rows.length)},minmax(0,1fr))` }} onPointerLeave={() => setHoverKey(null)}>
      {rows.map((row, index) => <button key={row.key} id={`${id}-${index}`} type="button" tabIndex={-1}
        className={`plot-column ${row.key === selectedKey ? 'is-selected' : ''} ${row.key === focusedKey ? 'is-focused' : ''}`}
        aria-label={`${row.day ?? ''} ${row.label}，${qualityText(row)}，代理合计 ${formatBytes(totalBytes(row.uploaded, row.downloaded))}`}
        data-testid={`column-${row.key}`} onPointerEnter={() => setHoverKey(row.key)} onPointerMove={() => setHoverKey(row.key)}
        onClick={(event) => { event.currentTarget.closest<HTMLElement>('.trend-chart')?.focus(); if (daily) setFocusKey(row.key); onSelect(row); }}>
        {row.gapSeconds > 0 && <span className="gap-mark" aria-hidden="true" />}
        {!row.coveredSeconds && !row.legacyCoverage && row.downloaded === '0' && row.uploaded === '0' && <span className="missing-mark" aria-hidden="true" />}
      </button>)}
    </div>
    {hovered && <div role="tooltip" className="chart-tooltip" style={{ left: tooltipLeft, width: tooltipWidth }}>
      <strong>{hovered.day && !daily ? `${hovered.day} ` : ''}{daily ? hovered.day ?? hovered.key : hovered.hour !== undefined ? hourPeriod(hovered.hour, null) : hovered.label}</strong>
      <div className="tooltip-total">代理合计 <b>{formatBytes(totalBytes(hovered.uploaded, hovered.downloaded))}</b></div>
      <div className="tooltip-parts"><span>下载 {formatBytes(hovered.downloaded)}</span><span>上传 {formatBytes(hovered.uploaded)}</span></div>
      <p>{qualityText(hovered)}</p>
    </div>}
  </div>;
}
