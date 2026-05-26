import http from 'k6/http';
import { SharedArray } from 'k6/data';
import exec from 'k6/execution';

const testData = new SharedArray('test-data', function () {
    return JSON.parse(open('./test-data.json')).entries;
});

export const options = {
    vus: 10,
    iterations: 1000,
    maxDuration: '30s',
};

export default function () {
    const idx = exec.scenario.iterationInTest;
    if (idx >= testData.length) return;
    const entry = testData[idx];

    http.post(
        'http://localhost:9999/fraud-score',
        JSON.stringify(entry.request),
        { headers: { 'Content-Type': 'application/json' }, timeout: '2s' }
    );
}
