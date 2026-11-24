import '@tracon/ui/index.css';
import { connectTheme } from '@tracon/ui';
import { mount } from 'svelte';
import App from './App.svelte';
import './style.css';

// Before the first paint: follow the theme of the Tracon interface framing
// this page, or the system when hora is opened standalone.
connectTheme();

const target = document.getElementById('app');
if (!target) throw new Error('Missing hora application root');
mount(App, { target });
