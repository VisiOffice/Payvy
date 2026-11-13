import { useEffect, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Center } from '@astryxdesign/core/Center';
import { Heading } from '@astryxdesign/core/Heading';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, auth, isChoice, type Choice, type Me } from '../api/client';

/**
 * Asks for a sign-in link, and opens one: the emailed link lands here with
 * its token after `#`, which never leaves the browser until it's posted.
 */
export function SignIn({ onSignedIn }: { onSignedIn: (me: Me) => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const requested = (location.state as { from?: string } | null)?.from ?? new URLSearchParams(location.search).get('return_to');
  const from = requested && /^\/bills\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(requested) ? requested : '/';
  const token = new URLSearchParams(location.hash.slice(1)).get('token');

  const [email, setEmail] = useState('');
  const [sent, setSent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [redeeming, setRedeeming] = useState(Boolean(token));
  const [choice, setChoice] = useState<Choice | null>(null);
  const redeemed = useRef(false);

  useEffect(() => {
    // Once, even under StrictMode's double effect: a link works only once.
    if (!token || redeemed.current) return;
    redeemed.current = true;
    // Take the token out of the address bar and history.
    window.history.replaceState(null, '', '/sign-in');
    auth
      .redeem(token)
      .then((r) => {
        if (isChoice(r)) {
          // The address belongs to several companies: pick one.
          setChoice(r);
          setRedeeming(false);
          return;
        }
        onSignedIn(r);
        navigate(from, { replace: true });
      })
      .catch((e) => {
        setError(e instanceof ApiError ? e.message : 'Signing in failed. Try again.');
        setRedeeming(false);
      });
  }, [token, onSignedIn, navigate, from]);

  async function ask() {
    setError(null);
    try {
      const r = await auth.requestLink(email, from);
      setSent(r.message);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Something went wrong. Try again.');
    }
  }

  async function pick(orgId: string) {
    if (!choice) return;
    setError(null);
    setRedeeming(true);
    try {
      const me = await auth.choose(choice.ticket, orgId);
      onSignedIn(me);
      navigate(from, { replace: true });
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Signing in failed. Try again.');
      setRedeeming(false);
      // An expired ticket can't pick again; start over with a new link.
      if (e instanceof ApiError && e.code === 'link_expired') setChoice(null);
    }
  }

  if (redeeming) {
    return (
      <Center height="100%">
        <Spinner size="lg" label="Signing you in" />
      </Center>
    );
  }

  return (
    <Center height="100%">
      <div>
        <div className="payvy-signin-brand">
          <div className="payvy-wordmark">
            Payvy<span>.</span>
          </div>
          <Text type="supporting">Your bills. Your bank. Your rules.</Text>
        </div>
        <Card width={400} maxWidth="calc(100vw - 32px)" padding={6}>
          <VStack gap={4}>
            <Heading level={1}>{choice ? 'Choose a company' : 'Sign in to Payvy'}</Heading>
            {error && <Banner status="error" title={error} />}
            {choice ? (
              <VStack gap={3}>
                <Text type="supporting">Your address belongs to more than one company. Pick one; you can switch later from the company menu.</Text>
                {choice.choose.map((c) => (
                  <Button key={c.id} label={c.name} variant="secondary" width="100%" clickAction={() => pick(c.id)} />
                ))}
              </VStack>
            ) : sent ? (
              <VStack gap={3}>
                <Text>{sent}</Text>
                <Text type="supporting">Check {email}. You can close this tab.</Text>
                <Button label="Use a different address" variant="ghost" onClick={() => setSent(null)} />
              </VStack>
            ) : (
              <VStack gap={3}>
                <Text type="supporting">We'll email you a link. No password needed.</Text>
                <TextInput
                  label="Work email"
                  type="email"
                  value={email}
                  onChange={setEmail}
                  onEnter={ask}
                  autoComplete="email"
                  hasAutoFocus
                />
                <Button
                  label="Email me a link"
                  variant="primary"
                  clickAction={ask}
                  isDisabled={!email.includes('@')}
                  width="100%"
                />
              </VStack>
            )}
            {from !== '/' && !sent && (
              <Text type="supporting">After signing in you'll go back to {from}.</Text>
            )}
          </VStack>
        </Card>
      </div>
    </Center>
  );
}
