"""Repeated-cutoff controls. Uses temporary data and supplied ASR, never a model."""
from __future__ import annotations
import copy, os, sys, tempfile, types, unittest
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch
import numpy as np
import soundfile as sf
_data=tempfile.TemporaryDirectory(prefix='vf-repeat-tests-')
os.environ.setdefault('VOICEFORGE_DATA',_data.name)
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from app import audio as au, config, ml, pipeline, studio
SR=24000

def tone(d,amp=.3):
    return (amp*np.sin(2*np.pi*200*np.arange(int(d*SR))/SR)).astype(np.float32)

def natural():
    a=tone(2);a[-SR//4:]*=np.geomspace(1,1e-5,SR//4)
    return np.pad(a,(0,SR//3))

class RepeatCutoffTests(unittest.TestCase):
    def setUp(self):
        import re
        self.normalizer=patch.object(ml,'normalize_for_wer',side_effect=lambda t:re.sub(r'[^\w\s]','',t.lower()).strip())
        self.normalizer.start();self.addCleanup(self.normalizer.stop)
        self.dir=Path(tempfile.mkdtemp(dir=config.GENERATIONS_DIR));self.addCleanup(lambda:__import__('shutil').rmtree(self.dir,ignore_errors=True))
        self.gen={'id':self.dir.name,'engine':'qwen3-tts-1.7b','language':'en','params':{},'profile_id':'test','chunks':[{'i':0,'text':'hello','status':'pending','pause_after':'end'}]}

    def render(self,gen,seen,report=None):
        a=natural()
        ev={'audio':a,'raw_audio':a,'sr':SR,'issues':[],'score':0.,'wer':0.,'sim':1.,'max_gap':0.,'dur':len(a)/SR,'heard':'hello','ending':{'natural':True}}
        def synth(*args,**kwargs):
            seen.append(args[6]);return a,SR
        with ExitStack() as p:
            for obj,name,kw in [
                (pipeline.engines,'get_spec',{'return_value':types.SimpleNamespace(family='qwen3')}),
                (pipeline.delivery,'resolve',{'return_value':({}, {})}),
                (pipeline.delivery,'pick_variant',{'return_value':0}),
                (pipeline,'profile_assets',{'return_value':(Path('ref'), 'ref',np.ones(1),1.,1.)}),
                (pipeline,'noise_limit',{'return_value':.1}),
                (pipeline,'_ref_pitch',{'return_value':None}),
                (pipeline.engines,'synthesis_report',{'return_value':report or {}}),
                (pipeline.engines,'synthesize',{'side_effect':synth}),
                (pipeline.ending_guard,'synthesis_text',{'return_value':('hello',False)}),
                (pipeline,'evaluate',{'side_effect':lambda *a,**k:copy.deepcopy(ev)}),
                (pipeline.au,'normalize_lufs',{'side_effect':lambda a,*args,**kw:a})]:
                p.enter_context(patch.object(obj,name,**kw))
            return pipeline.render_chunk(gen,0,{}, {**config.DEFAULT_SETTINGS,'max_attempts':1})

    def _render_with_clips(self, issues):
        """Three takes, every one judged with `issues`; the voice has two clips. Returns the clip used per take."""
        a = natural()
        ev = {'audio': a, 'raw_audio': a, 'sr': SR, 'issues': issues, 'score': 3., 'wer': 0., 'sim': 1., 'max_gap': 0.,
              'dur': len(a) / SR, 'heard': 'hello', 'ending': {'natural': not issues}}
        clips = []
        def assets(profile, variant=None):
            clips.append(variant); return Path(f'ref{variant}'), 'ref', np.ones(1), 1., 1.
        profile = {'id': 'test', 'active_variant': 0, 'variants': [{'index': 0, 'score': .9}, {'index': 1, 'score': .8}]}
        with ExitStack() as p:
            for obj, name, kw in [
                (pipeline.engines, 'get_spec', {'return_value': types.SimpleNamespace(family='qwen3')}),
                (pipeline.delivery, 'resolve', {'return_value': ({}, {})}),
                (pipeline.delivery, 'pick_variant', {'return_value': 0}),
                (pipeline, 'profile_assets', {'side_effect': assets}),
                (pipeline, 'noise_limit', {'return_value': .1}),
                (pipeline, '_ref_pitch', {'return_value': None}),
                (pipeline.engines, 'synthesis_report', {'return_value': {}}),
                (pipeline.engines, 'synthesize', {'return_value': (a, SR)}),
                (pipeline.ending_guard, 'synthesis_text', {'return_value': ('hello', False)}),
                (pipeline, 'evaluate', {'side_effect': lambda *x, **k: copy.deepcopy(ev)}),
                (pipeline.au, 'normalize_lufs', {'side_effect': lambda a, *args, **kw: a})]:
                p.enter_context(patch.object(obj, name, **kw))
            ch = pipeline.render_chunk(self.gen, 0, profile, {**config.DEFAULT_SETTINGS, 'max_attempts': 3, 'compare_takes': 1})
        return [t['ref'] for t in ch['attempts']]

    def test_when_every_take_fails_the_last_one_uses_another_clip(self):
        # Goal step 7: retakes used to change only the seed. Step 6: a different clean clip, 0 of 20 flagged vs 3 of 20.
        self.assertEqual(self._render_with_clips(['cut_off']), [0, 0, 1])

    def test_a_passing_take_keeps_the_chosen_clip(self):
        self.assertEqual(self._render_with_clips([]), [0])

    def test_two_generations_do_not_repeat_the_same_take_seed(self):
        other=copy.deepcopy(self.gen);other['id']=self.dir.name+'-other';(config.GENERATIONS_DIR/other['id']).mkdir()
        self.addCleanup(lambda:__import__('shutil').rmtree(config.GENERATIONS_DIR/other['id'],ignore_errors=True))
        seeds=[];self.render(self.gen,seeds);self.render(other,seeds)
        self.assertNotEqual(seeds[0],seeds[1])

    def test_repeated_render_even_without_regen_counter_has_new_seed(self):
        seeds=[];self.render(self.gen,seeds);self.render(self.gen,seeds)
        self.assertNotEqual(seeds[0],seeds[1])

    def test_render_avoids_previous_seed_even_if_random_nonce_repeats(self):
        seeds=[]
        with patch.object(pipeline.secrets,'randbits',return_value=100):
            self.render(self.gen,seeds);self.render(self.gen,seeds)
        self.assertNotEqual(seeds[0],seeds[1])

    def test_learned_duration_budget_survives_a_later_render(self):
        seeds=[]
        ch=self.render(self.gen,seeds,{'stop_reason':'length_limit_reached'})
        initial=ch['attempts'][0]['expected_seconds']
        self.assertGreater(ch['duration_budget'],1.)
        ch=self.render(self.gen,seeds)
        self.assertGreater(ch['attempts'][0]['expected_seconds'],initial)

    def test_quiet_first_sound_is_not_trimmed_or_faded(self):
        a=np.concatenate([tone(.3,.3*10**(-45/20)),tone(1.7),np.full(SR//20,.001,np.float32)])
        tr={'text':'hello','segments':[{'words':[{'word':'hello','start':0.,'end':2.}]}]}
        with patch.object(ml,'transcribe',return_value=tr),patch.object(ml,'speaker_embedding',return_value=np.ones(1)),patch.object(ml,'cosine',return_value=1.):
            ev=pipeline.evaluate(a,SR,'hello',np.ones(1),1.,.5,config.DEFAULT_SETTINGS,'en')
        pad=round(ev['ending'].get('head_pad_s',0)*SR)
        np.testing.assert_allclose(ev['audio'][pad:pad+int(.3*SR)],a[:int(.3*SR)],atol=1e-8)

    def test_quiet_tail_is_kept_past_old_scan_limit(self):
        a=np.concatenate([tone(1.),tone(.7,.3*10**(-30/20)),np.zeros(SR//3,np.float32)])
        e=au.end_analysis(a,SR,1.)
        self.assertGreaterEqual(e['cut'],1.7)
        self.assertTrue(e['natural'])

    def test_one_quiet_frame_does_not_hide_an_abrupt_model_stop(self):
        # Crow part 5: final active frames were -13.7 and -36.4 dB relative
        # to speech. Their dB mean passed despite a loud, abrupt ending.
        a=np.concatenate([tone(1.),tone(.01,.3*10**(-13.7/20)),
                          tone(.01,.3*10**(-36.4/20)),np.zeros(SR//10)])
        self.assertFalse(au.end_analysis(a,SR,1.)['natural'])

    def test_evaluate_keeps_a_short_release_after_a_long_closure(self):
        release=tone(.06)
        release*=np.geomspace(1,1e-5,len(release))
        a=np.concatenate([natural()[:2*SR],np.zeros(int(.3*SR)),release,np.zeros(SR//3)])
        tr={'text':'hello','segments':[{'words':[{'word':'hello','start':0.,'end':2.}]}]}
        with patch.object(ml,'speaker_embedding',return_value=np.ones(1)),patch.object(ml,'cosine',return_value=1.):
            ev=pipeline.evaluate(a,SR,'hello',np.ones(1),1.,.5,config.DEFAULT_SETTINGS,'en',transcript=tr)
        self.assertFalse(ev['ending']['junk'])
        self.assertNotIn('cut_off',ev['issues'])
        pad=round(ev['ending']['head_pad_s']*SR)
        lo=round(2.3*SR);hi=lo+len(release)
        np.testing.assert_array_equal(ev['audio'][pad+lo:pad+hi],a[lo:hi])

    def test_final_check_rejects_a_tail_lost_before_assembly(self):
        a=np.concatenate([tone(1.),tone(.7,.3*10**(-30/20)),np.zeros(SR//3)])
        # The export perfectly matches an already damaged chunk, hiding the loss.
        chunk=np.pad(a[:int(1.2*SR)],(int(.02*SR),int(.12*SR)))
        sf.write(self.dir/'raw.wav',a,SR,subtype='FLOAT')
        sf.write(self.dir/'chunk.wav',chunk,SR)
        sf.write(self.dir/'final.wav',chunk,SR)
        self.gen['chunks'][0].update(raw_file='raw.wav',file='chunk.wav',issues=[],
                                    ending={'last_word_end':1.,'cut':1.2,'trim_start':0.,'head_pad_s':.02})
        self.gen['timeline']=[{'i':0,'start':0,'end':len(chunk)/SR}]
        self.assertEqual(pipeline.final_scan(self.gen),[0])
        self.assertIn('processed_audio_cut_off',self.gen['chunks'][0]['final_issues'])

    def test_joining_legacy_unpadded_chunks_does_not_fade_spoken_samples(self):
        a=tone(.1)
        np.testing.assert_array_equal(au.concat_with_pauses([a,a],[0,0],SR),np.concatenate([a,a]))

    def test_raw_clip_partial_final_frame_is_preserved(self):
        a=tone(1.705)
        self.assertAlmostEqual(au.end_analysis(a,SR,1.6)['cut'],len(a)/SR)

    def test_missing_first_word_is_a_hard_issue_in_real_evaluate(self):
        a=natural();text='When the long day was finally over they asked me to come home.'
        heard=text.removeprefix('When ')
        tr={'text':heard,'segments':[{'words':[{'word':w,'start':i*.12,'end':(i+1)*.12} for i,w in enumerate(heard.split())]}]}
        with patch.object(ml,'transcribe',return_value=tr),patch.object(ml,'speaker_embedding',return_value=np.ones(1)),patch.object(ml,'cosine',return_value=1.):
            ev=pipeline.evaluate(a,SR,text,np.ones(1),1.,6.,config.DEFAULT_SETTINGS,'en')
        self.assertIn('start_word_missing',ev['issues'])
        self.assertIn('start_word_missing',pipeline.HARD_ISSUES)

    def test_missing_first_word_cannot_pass_low_overall_wer(self):
        text='When the long day was finally over they asked me to come home.'
        heard='the long day was finally over they asked me to come home.'
        self.assertLess(ml.wer(text,heard),config.DEFAULT_SETTINGS['max_wer'])
        self.assertTrue(pipeline._first_word_missing(text,heard))
        self.assertFalse(pipeline._first_word_missing(text,text))

    def test_previous_come_does_not_cover_missing_home(self):
        text='When the long day was finally over they asked me to come home.'
        self.assertTrue(pipeline._final_word_missing(text,text.replace(' home.','.')))
        self.assertFalse(pipeline._final_word_missing(text,text))

    def test_split_names_still_match_at_both_edges(self):
        self.assertFalse(pipeline._first_word_missing('Scadutree stood tall.','Skadu tree stood tall.'))
        self.assertFalse(pipeline._final_word_missing('Beneath the Scadutree.','Beneath the Skadu tree.'))

    def test_short_expander_keeps_shape_without_crashing(self):
        for d in (.1,.3,.55,.6,.61):
            a=tone(d);self.assertEqual(studio.expander(a,SR).shape,a.shape)

    def test_final_check_rejects_a_silent_processed_export(self):
        a=natural();sf.write(self.dir/'raw.wav',a,SR,subtype='FLOAT');sf.write(self.dir/'chunk.wav',a,SR)
        sf.write(self.dir/'final.wav',np.zeros_like(a),SR)
        self.gen['chunks'][0].update(raw_file='raw.wav',file='chunk.wav',issues=[],ending={'last_word_end':2.})
        self.gen['timeline']=[{'i':0,'start':0,'end':len(a)/SR}]
        self.assertEqual(pipeline.final_scan(self.gen),[0])

    def test_final_check_accepts_undamaged_processed_export(self):
        a=natural();sf.write(self.dir/'raw.wav',a,SR,subtype='FLOAT');sf.write(self.dir/'chunk.wav',a,SR);sf.write(self.dir/'final.wav',a,SR)
        self.gen['chunks'][0].update(raw_file='raw.wav',file='chunk.wav',issues=[],ending={'last_word_end':2.})
        self.gen['timeline']=[{'i':0,'start':0,'end':len(a)/SR}]
        self.assertEqual(pipeline.final_scan(self.gen),[])

    def test_final_check_rejects_a_processed_tail_removed_after_good_raw(self):
        a=natural();damaged=a.copy();damaged[int(1.65*SR):]=0
        sf.write(self.dir/'raw.wav',a,SR,subtype='FLOAT');sf.write(self.dir/'chunk.wav',a,SR);sf.write(self.dir/'final.wav',damaged,SR)
        self.gen['chunks'][0].update(raw_file='raw.wav',file='chunk.wav',issues=[],ending={'last_word_end':2.})
        self.gen['timeline']=[{'i':0,'start':0,'end':len(a)/SR}]
        self.assertEqual(pipeline.final_scan(self.gen),[0])
        self.assertIn('processed_audio_cut_off',self.gen['chunks'][0]['final_issues'])

    def test_final_check_accepts_real_atempo_speed_changes(self):
        import subprocess
        a=natural();sf.write(self.dir/'raw.wav',a,SR,subtype='FLOAT');sf.write(self.dir/'chunk.wav',a,SR)
        self.gen['chunks'][0].update(raw_file='raw.wav',file='chunk.wav',issues=[],ending={'last_word_end':2.})
        for speed in (.7,.85,1.2,1.3):
            subprocess.run(['ffmpeg','-y','-loglevel','error','-i',str(self.dir/'chunk.wav'),'-af',f'atempo={speed}',str(self.dir/'final.wav')],check=True)
            self.gen['speed']=speed;self.gen['timeline']=[{'i':0,'start':0,'end':len(a)/SR/speed}]
            self.assertEqual(pipeline.final_scan(self.gen),[],f'speed={speed}')

if __name__=='__main__':unittest.main()
