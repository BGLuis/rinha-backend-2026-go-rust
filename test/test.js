import http from 'k6/http';
import { check } from 'k6';
import { SharedArray } from 'k6/data';
import { Counter } from 'k6/metrics';
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.0.1/index.js';
import exec from 'k6/execution';

const testData = new SharedArray('test-data', function () {
	return JSON.parse(open('./test-data.json')).entries;
});
const statsArr = new SharedArray('test-stats', function () {
	return [JSON.parse(open('./test-data.json')).stats];
});
const expectedStats = statsArr[0];

const tpCount = new Counter('tp_count');
const tnCount = new Counter('tn_count');
const fpCount = new Counter('fp_count');
const fnCount = new Counter('fn_count');
const errorCount = new Counter('error_count');

export const options = {
	summaryTrendStats: ['p(99)'],
	systemTags: ['status', 'method'],
	dns: {
		ttl: '5m',
		select: 'roundRobin',
	},
	scenarios: {
		// Cenário 1: Carga Base Constante (Simula tráfego normal)
		base_load: {
			executor: 'constant-arrival-rate',
			rate: 1000,
			timeUnit: '1s',
			duration: '120s',
			preAllocatedVUs: 200,
			maxVUs: 1500,
		},
		// Cenário 2: Picos Agressivos Paralelos (Stress e Elasticidade)
		spike_load: {
			executor: 'ramping-arrival-rate',
			startRate: 0,
			timeUnit: '1s',
			startTime: '10s', // Inicia 10s após a base
			preAllocatedVUs: 100,
			maxVUs: 4000,
			stages: [
				{ duration: '10s', target: 4000 }, // Pico rápido (Spike)
				{ duration: '15s', target: 4000 }, // Sustenta o pico
				{ duration: '15s', target: 1000 }, // Desce rápido
				{ duration: '20s', target: 8000 }, // Tsunami (Stress extremo)
				{ duration: '30s', target: 8000 }, // Sustenta o Tsunami
				{ duration: '20s', target: 0 },    // Cooldown
			],
		},
	},
};

export function setup() {
	console.log(
		`Dataset: ${expectedStats.total} entries, ` +
			`${expectedStats.fraud_count} fraud (${expectedStats.fraud_rate}%), ` +
			`${expectedStats.legit_count} legit (${expectedStats.legit_rate}%), ` +
			`edge cases: ${expectedStats.edge_case_rate}%`,
	);
}

export default function () {
	// Usa módulo para ciclar pelo array indefinidamente durante a carga agressiva
	const idx = exec.scenario.iterationInTest % testData.length;
	const entry = testData[idx];
	const expectedApproved = entry.expected_approved;

	const res = http.post(
		'http://localhost:9999/fraud-score',
		JSON.stringify(entry.request),
		{ headers: { 'Content-Type': 'application/json' }, timeout: '2001ms' },
	);

	if (res.status === 200) {
		const body = JSON.parse(res.body);
		// Per-request scoring: compare against expectedApproved
		// expectedApproved === true  --> legit transaction
		// expectedApproved === false --> fraud transaction
		if (expectedApproved === body.approved) {
			if (body.approved)
				tnCount.add(1); // correctly approved legit
			else tpCount.add(1); // correctly denied fraud
		} else {
			if (body.approved)
				fnCount.add(1); // fraud approved (missed fraud)
			else fpCount.add(1); // legit denied (false block)
		}
	} else {
		errorCount.add(1);
	}
}

export function handleSummary(data) {
	const K = 1000;
	const T_MAX_MS = 1000;
	const P99_MIN_MS = 1;
	const P99_MAX_MS = 2000;
	const EPSILON_MIN = 0.001;
	const BETA = 300;
	const TX_CORTE = 0.15;
	const SCORE_P99_CORTE = -3000;
	const SCORE_DET_CORTE = -3000;

	const httpDuration = data.metrics.http_req_duration.values;
	const p99 = httpDuration['p(99)'];

	const tp = data.metrics.tp_count ? data.metrics.tp_count.values.count : 0;
	const tn = data.metrics.tn_count ? data.metrics.tn_count.values.count : 0;
	const fp = data.metrics.fp_count ? data.metrics.fp_count.values.count : 0;
	const fn = data.metrics.fn_count ? data.metrics.fn_count.values.count : 0;
	const errs = data.metrics.error_count
		? data.metrics.error_count.values.count
		: 0;

	const N = tp + tn + fp + fn + errs;

	// Erros ponderados (para a fórmula log) e contagem pura (para o corte)
	const E = fp * 1 + fn * 3 + errs * 5;
	const failures = fp + fn + errs;
	const epsilon = N > 0 ? E / N : 0;
	const failureRate = N > 0 ? failures / N : 0;

	// Score P99 (log, com teto em P99_MIN_MS e corte em P99_MAX_MS).
	// p99=0 = nenhuma resposta completou; retorna 0 pra evitar Infinity no JSON.
	let p99Score;
	let p99CutTriggered = false;
	if (p99 <= 0) {
		p99Score = 0;
	} else if (p99 > P99_MAX_MS) {
		p99Score = SCORE_P99_CORTE;
		p99CutTriggered = true;
	} else {
		p99Score = K * Math.log10(T_MAX_MS / Math.max(p99, P99_MIN_MS));
	}

	// Score detecção (log com penalidade absoluta, ou corte em -3000 se falhas > 15%)
	let detScore;
	let rateComponent = 0;
	let absolutePenalty = 0;
	let cutTriggered = false;
	if (failureRate > TX_CORTE) {
		detScore = SCORE_DET_CORTE;
		cutTriggered = true;
	} else {
		rateComponent = K * Math.log10(1 / Math.max(epsilon, EPSILON_MIN));
		absolutePenalty = -BETA * Math.log10(1 + E);
		detScore = rateComponent + absolutePenalty;
	}

	const finalScore = p99Score + detScore;

	const result = {
		expected: expectedStats,
		p99: p99.toFixed(2) + 'ms',
		scoring: {
			breakdown: {
				false_positive_detections: fp,
				false_negative_detections: fn,
				true_positive_detections: tp,
				true_negative_detections: tn,
				http_errors: errs,
			},
			failure_rate: +(failureRate * 100).toFixed(2) + '%',
			weighted_errors_E: E,
			error_rate_epsilon: +epsilon.toFixed(6),
			p99_score: {
				value: +p99Score.toFixed(2),
				cut_triggered: p99CutTriggered,
			},
			detection_score: {
				value: +detScore.toFixed(2),
				rate_component: cutTriggered ? null : +rateComponent.toFixed(2),
				absolute_penalty: cutTriggered
					? null
					: +absolutePenalty.toFixed(2),
				cut_triggered: cutTriggered,
			},
			final_score: +finalScore.toFixed(2),
		},
	};

	return {
		'/test/results.json': JSON.stringify(result, null, 2),
		stdout: textSummary(data, { indent: ' ', enableColors: true }),
	};
}
