import sys
import time
import logging
from urllib.parse import urljoin

import requests
from requests.exceptions import RequestException, ConnectionError, Timeout


def http_boot(server_url: str, retries: int = 3, timeout: int = 10) -> None:
    """Download the workload from the dispatch server with basic error handling.

    This function performs an initial HEAD (when available) to learn the
    content-length, then streams the GET. It retries transient errors and
    prints helpful messages. It intentionally does not perform any "boot"
    actions; keep that separate from this client.
    """
    sess = requests.Session()

    # Try HEAD to learn content-length. If HEAD is not allowed or fails, we
    # will still attempt a GET and proceed without content-length.
    content_length = None
    for attempt in range(1, retries + 1):
        try:
            resp = sess.head(server_url, timeout=timeout)
            if resp.status_code < 400:
                cl = resp.headers.get('content-length')
                if cl is not None:
                    try:
                        content_length = int(cl)
                    except ValueError:
                        logging.debug('invalid content-length header: %r', cl)
                logging.info('HEAD ok (status=%s)', resp.status_code)
            else:
                logging.info('HEAD returned status %s; will try GET', resp.status_code)
            break
        except (ConnectionError, Timeout) as e:
            logging.warning('HEAD attempt %d/%d failed: %s', attempt, retries, e)
            if attempt == retries:
                logging.error('HEAD failed after %d attempts, falling back to GET', retries)
            else:
                time.sleep(1 * attempt)
        except RequestException as e:
            logging.error('HEAD failed: %s', e)
            break

    # Now perform the GET and stream the body
    print('Starting download from:', server_url)
    try:
        get_resp = sess.get(server_url, stream=True, timeout=timeout)
    except (ConnectionError, Timeout) as e:
        logging.error('GET failed: %s', e)
        sys.exit(2)
    except RequestException as e:
        logging.error('GET failed: %s', e)
        sys.exit(1)

    if get_resp.status_code >= 400:
        logging.error('Server returned HTTP %s; aborting', get_resp.status_code)
        sys.exit(3)

    # If we didn't get a content-length from HEAD, try to get it from GET
    if content_length is None:
        cl = get_resp.headers.get('content-length')
        if cl is not None:
            try:
                content_length = int(cl)
            except ValueError:
                logging.debug('invalid content-length header in GET: %r', cl)

    print('Downloading workload...')
    downloaded = 0
    chunk_size = 8192
    try:
        for chunk in get_resp.iter_content(chunk_size=chunk_size):
            if not chunk:
                continue
            downloaded += len(chunk)
            if content_length:
                print(f'Downloaded: {downloaded}/{content_length} bytes', end='\r')
            else:
                print(f'Downloaded: {downloaded} bytes', end='\r')
            # small sleep to make demo output readable; remove or shorten for real runs
            time.sleep(0.05)
    except (ConnectionError, Timeout) as e:
        logging.error('\nDownload interrupted: %s', e)
        sys.exit(4)
    except RequestException as e:
        logging.error('\nError while downloading: %s', e)
        sys.exit(5)

    print('\nDownload complete!')

import argparse

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="HTTP boot client for dispatch")
    parser.add_argument('--host', default='localhost', help='Dispatch server host (default: localhost)')
    parser.add_argument('--port', type=int, default=58080, help='Dispatch server port (default: 58080)')
    args = parser.parse_args()

    server = f"http://{args.host}:{args.port}/dispatch"
    http_boot(server)
    