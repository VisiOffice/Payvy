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
import { ApiError, auth, type Me } from '../api/client';

/**
 * Asks for a sign-in link, and opens one: the emailed link lands here with
 * its token after `#`, which never leaves the browser until it's posted.
 */
export function SignIn({ onSignedIn }: { onSignedIn: (me: Me) => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const from = (location.state as { from?: string } | null)?.from ?? '/';
  const token = new URLSearchParams(location.hash.slice(1)).get('token');

  const [email, setEmail] = useState('');
  const [sent, setSent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [redeeming, setRedeeming] = useState(Boolean(token));
  const redeemed = useRef(false);

  useEffect(() => {
    // Once, even under StrictMode's double effect: a link works only once.
    if (!token || redeemed.current) return;
    redeemed.current = true;
    // Take the token out of the address bar and history.
    window.history.replaceState(null, '', '/sign-in');
    auth
      .redeem(token)
      .then((me) => {
        onSignedIn(me);
        navigate('/', { replace: true });
      })
      .catch((e) => {
        setError(e instanceof ApiError ? e.message : 'Signing in failed. Try again.');
        setRedeeming(false);
      });
  }, [token, onSignedIn, navigate]);

  async function ask() {
    setError(null);
    try {
      const r = await auth.requestLink(email);
      setSent(r.message);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Something went wrong. Try again.');
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
      <Card width={400} maxWidth="calc(100vw - 32px)" padding={6}>
        <VStack gap={4}>
          <Heading level={1}>Sign in to Payvy</Heading>
          {error && <Banner status="error" title={error} />}
          {sent ? (
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
    </Center>
  );
}
