import '@fontsource-variable/inter';
import './app.css';
import { mount } from 'svelte';
import { apiPromise } from './lib/api';
import App from './App.svelte';

apiPromise.then(() => {
  mount(App, { target: document.getElementById('app')! });
});
